//! 开发环境：Docker、iOS 模拟器占的空间。只读；清理在 `remove::clean_devenv`（规格切片 17）。

use crate::i18n::t;
use serde::Serialize;
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, UNIX_EPOCH};

pub const XCRUN: &str = "/usr/bin/xcrun";
/// 读取的超时：Docker 守护进程刚启动时 `docker system df` 会卡住
pub const READ_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct Usage {
    pub size_bytes: u64,
    pub reclaimable_bytes: u64,
    pub count: u32,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Docker {
    NotInstalled,
    NotRunning,
    Ready { images: Usage, containers: Usage, volumes: Usage, build_cache: Usage },
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct SimDevice {
    pub udid: String,
    pub name: String,
    /// 系统版本的标识，例如 `com.apple.CoreSimulator.SimRuntime.iOS-18-3`
    pub runtime: String,
    /// 给人看的系统版本，例如 `iOS 18.3`
    pub os: String,
    pub available: bool,
    pub size_bytes: u64,
    /// `simctl` 不给上次启动时间，用数据目录的修改时间代替
    pub last_used_ms: Option<u64>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct SimRuntime {
    /// `simctl runtime delete` 用的编号（UUID）
    pub id: String,
    pub runtime: String,
    pub os: String,
    pub version: String,
    pub size_bytes: u64,
    /// 随 Xcode 安装的不能单独删
    pub deletable: bool,
    /// 有设备在用这个版本
    pub in_use: bool,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Simulators {
    pub devices: Vec<SimDevice>,
    pub runtimes: Vec<SimRuntime>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct DevEnvReport {
    pub docker: Docker,
    /// 没装 Xcode 时为空
    pub simulators: Option<Simulators>,
}

/// 跑一条命令，超时就杀掉。返回标准输出；退出码不为 0 时返回标准错误。
/// 输出边跑边读：等进程退出再读的话，输出超过管道缓冲区（64 KB）时进程会卡住写不出去。
pub fn run(bin: &Path, args: &[&str], timeout: Duration) -> Result<String, String> {
    let mut child = Command::new(bin).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
    let drain = |mut pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    };
    let out = drain(Box::new(child.stdout.take().unwrap()));
    let err = drain(Box::new(child.stderr.take().unwrap()));
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(t("超时了").into());
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let (out, err) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
    if status.success() {
        Ok(String::from_utf8_lossy(&out).into_owned())
    } else {
        Err(String::from_utf8_lossy(&err).trim().to_owned())
    }
}

/// Docker 的大小写法：`1.106GB (2%)`、`462.7MB`、`12.5kB`、`0B`，按 1000 进位。
pub fn parse_size(s: &str) -> u64 {
    let token = s.split_whitespace().next().unwrap_or("");
    let split = token.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(token.len());
    let (num, unit) = token.split_at(split);
    let n: f64 = num.parse().unwrap_or(0.0);
    let mul = match unit.to_ascii_uppercase().as_str() {
        "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        _ => 1.0,
    };
    (n * mul).round() as u64
}

/// `docker system df --format json`：每行一个 JSON。
pub fn parse_docker_df(out: &str) -> Docker {
    let mut usage: [Usage; 4] = Default::default();
    for line in out.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let i = match v["Type"].as_str() {
            Some("Images") => 0,
            Some("Containers") => 1,
            Some("Local Volumes") => 2,
            Some("Build Cache") => 3,
            _ => continue,
        };
        usage[i] = Usage {
            size_bytes: parse_size(v["Size"].as_str().unwrap_or("")),
            reclaimable_bytes: parse_size(v["Reclaimable"].as_str().unwrap_or("")),
            count: v["TotalCount"].as_str().and_then(|c| c.parse().ok()).unwrap_or(0),
        };
    }
    let [images, containers, volumes, build_cache] = usage;
    Docker::Ready { images, containers, volumes, build_cache }
}

/// `com.apple.CoreSimulator.SimRuntime.iOS-18-3` → `iOS 18.3`
pub fn runtime_label(runtime: &str) -> String {
    let last = runtime.rsplit('.').next().unwrap_or(runtime);
    match last.split_once('-') {
        Some((os, version)) => format!("{os} {}", version.replace('-', ".")),
        None => last.to_owned(),
    }
}

/// `xcrun simctl list devices --json`。按大小降序。
pub fn parse_devices(out: &str, modified_ms: &dyn Fn(&Path) -> Option<u64>) -> Vec<SimDevice> {
    let Ok(v) = serde_json::from_str::<Value>(out) else { return Vec::new() };
    let Some(groups) = v["devices"].as_object() else { return Vec::new() };
    let mut devices = Vec::new();
    for (runtime, list) in groups {
        for d in list.as_array().into_iter().flatten() {
            let (Some(udid), Some(name)) = (d["udid"].as_str(), d["name"].as_str()) else { continue };
            devices.push(SimDevice {
                udid: udid.to_owned(),
                name: name.to_owned(),
                runtime: runtime.clone(),
                os: runtime_label(runtime),
                available: d["isAvailable"].as_bool().unwrap_or(false),
                size_bytes: d["dataPathSize"].as_u64().unwrap_or(0),
                last_used_ms: d["dataPath"].as_str().and_then(|p| modified_ms(Path::new(p))),
            });
        }
    }
    devices.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
    devices
}

/// `xcrun simctl runtime list -j`：以编号为键的对象。格式未实测（规格切片 17），不认识就返回空。
pub fn parse_runtimes(out: &str, devices: &[SimDevice]) -> Vec<SimRuntime> {
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(out) else { return Vec::new() };
    let mut runtimes: Vec<SimRuntime> = map
        .values()
        .filter_map(|r| {
            let id = r["identifier"].as_str()?.to_owned();
            let runtime = r["runtimeIdentifier"].as_str().unwrap_or("").to_owned();
            Some(SimRuntime {
                id,
                os: runtime_label(&runtime),
                in_use: devices.iter().any(|d| d.runtime == runtime),
                version: r["version"].as_str().unwrap_or("").to_owned(),
                size_bytes: r["sizeBytes"].as_u64().unwrap_or(0),
                deletable: r["deletable"].as_bool().unwrap_or(false),
                runtime,
            })
        })
        .collect();
    runtimes.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
    runtimes
}

/// 能清理的部分合计，用来算清理前后差了多少。不含 Docker 数据卷。
pub fn total(r: &DevEnvReport) -> u64 {
    docker_total(&r.docker).unwrap_or(0) + r.simulators.as_ref().map_or(0, sims_total)
}

/// 读不到（没装、没开、没响应）时为 None
fn docker_total(d: &Docker) -> Option<u64> {
    match d {
        Docker::Ready { images, containers, build_cache, .. } => Some(images.size_bytes + containers.size_bytes + build_cache.size_bytes),
        _ => None,
    }
}

fn sims_total(s: &Simulators) -> u64 {
    s.devices.iter().map(|d| d.size_bytes).sum::<u64>() + s.runtimes.iter().map(|x| x.size_bytes).sum::<u64>()
}

/// 清理腾出了多少：清理前后各读一次取差值。清理后某一部分读不到（Docker 没响应、模拟器列表读失败），
/// 不能把那一部分的全部大小当成腾出的，改用清理前所选目标的「可回收」估计。返回（大小, 是不是估计）。
pub fn freed(before: &DevEnvReport, after: &DevEnvReport, targets: &[crate::remove::Target]) -> (u64, bool) {
    use crate::remove::Target;
    let mut estimated = false;
    let docker = match (docker_total(&before.docker), docker_total(&after.docker), &before.docker) {
        (Some(b), Some(a), _) => b.saturating_sub(a),
        (Some(_), None, Docker::Ready { images, containers, build_cache, .. }) => {
            estimated = true;
            targets
                .iter()
                .map(|t| match t {
                    Target::DockerBuildCache => build_cache.reclaimable_bytes,
                    Target::DockerStoppedContainers => containers.reclaimable_bytes,
                    Target::DockerUnusedImages => images.reclaimable_bytes,
                    _ => 0,
                })
                .sum()
        }
        _ => 0,
    };
    let sims = match (&before.simulators, &after.simulators) {
        (Some(b), Some(a)) => sims_total(b).saturating_sub(sims_total(a)),
        (Some(b), None) => {
            estimated = true;
            targets
                .iter()
                .map(|t| match t {
                    Target::SimUnavailable => b.devices.iter().filter(|d| !d.available).map(|d| d.size_bytes).sum(),
                    Target::SimDevice(id) => b.devices.iter().filter(|d| &d.udid == id).map(|d| d.size_bytes).sum(),
                    Target::SimRuntime(id) => b.runtimes.iter().filter(|r| &r.id == id).map(|r| r.size_bytes).sum(),
                    _ => 0,
                })
                .sum()
        }
        _ => 0,
    };
    (docker + sims, estimated)
}

/// 空间去向里这两处显示「去清理」，跳到清理页的「开发环境」
pub fn cleanable_dirs(home: &Path) -> Vec<PathBuf> {
    vec![home.join("Library/Containers/com.docker.docker"), home.join("Library/Developer/CoreSimulator")]
}

pub fn report(home: &Path) -> DevEnvReport {
    // docker 和 xcrun 互不相干，一起跑：耗时取最长的那条，而不是几条相加
    std::thread::scope(|s| {
        let docker = s.spawn(docker);
        let simulators = simulators(home);
        DevEnvReport { docker: docker.join().unwrap_or(Docker::NotRunning), simulators }
    })
}

fn docker() -> Docker {
    let Some(bin) = crate::ai::locate("docker") else { return Docker::NotInstalled };
    match run(&bin, &["system", "df", "--format", "json"], READ_TIMEOUT) {
        Ok(out) => parse_docker_df(&out),
        // 装了但守护进程没开：命令报错或卡住
        Err(_) => Docker::NotRunning,
    }
}

/// 没用过模拟器时返回 None，不调 `xcrun`：没装开发者工具时 `/usr/bin/xcrun` 是系统的占位程序，
/// 一调用就弹「安装命令行开发者工具」的系统框，而不是安静地失败。
fn simulators(home: &Path) -> Option<Simulators> {
    if !home.join("Library/Developer/CoreSimulator/Devices").is_dir() {
        return None;
    }
    let xcrun = Path::new(XCRUN);
    // 两条命令一起跑；系统版本的解析要用到设备列表，所以解析放在两条都跑完之后
    let (out, runtimes) = std::thread::scope(|s| {
        let runtimes = s.spawn(|| run(xcrun, &["simctl", "runtime", "list", "-j"], READ_TIMEOUT));
        (run(xcrun, &["simctl", "list", "devices", "--json"], READ_TIMEOUT), runtimes.join())
    });
    let devices = parse_devices(&out.ok()?, &modified_ms);
    let runtimes = runtimes.ok().and_then(Result::ok).map(|o| parse_runtimes(&o, &devices)).unwrap_or_default();
    Some(Simulators { devices, runtimes })
}

fn modified_ms(p: &Path) -> Option<u64> {
    p.metadata().ok()?.modified().ok()?.duration_since(UNIX_EPOCH).ok().map(|d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::testutil::TempTree;

    // 本机 2026-10-15 `docker system df --format json` 的真实输出
    const DF: &str = r#"{"Active":"22","Reclaimable":"1.106GB (2%)","Size":"42.03GB","TotalCount":"30","Type":"Images"}
{"Active":"19","Reclaimable":"4.321MB (0%)","Size":"462.7MB","TotalCount":"22","Type":"Containers"}
{"Active":"13","Reclaimable":"129.4MB (6%)","Size":"1.874GB","TotalCount":"14","Type":"Local Volumes"}
{"Active":"0","Reclaimable":"709.7MB","Size":"1.467GB","TotalCount":"73","Type":"Build Cache"}"#;

    // 按本机 `simctl list devices --json` 的结构截取，编号和路径换成示例
    const DEVICES: &str = r#"{"devices":{
      "com.apple.CoreSimulator.SimRuntime.iOS-18-3":[
        {"availabilityError":"runtime profile not found","dataPath":"/tmp/a","dataPathSize":18337792,"udid":"5C0983CC-6196-45A4-ABD3-5DCE776893EC","isAvailable":false,"name":"iPhone 16"}],
      "com.apple.CoreSimulator.SimRuntime.iOS-26-0":[
        {"dataPath":"/tmp/b","dataPathSize":2000000000,"udid":"0A1B2C3D-0000-4000-8000-000000000001","isAvailable":true,"name":"iPhone 17 Pro"}]}}"#;

    // 未实测：按 Apple 的 `simctl runtime list -j` 常见结构写的样本，实测后以真实输出为准
    const RUNTIMES: &str = r#"{
      "AAAAAAAA-0000-4000-8000-000000000001":{"identifier":"AAAAAAAA-0000-4000-8000-000000000001","runtimeIdentifier":"com.apple.CoreSimulator.SimRuntime.iOS-26-0","version":"26.0","sizeBytes":7500000000,"deletable":true},
      "AAAAAAAA-0000-4000-8000-000000000002":{"identifier":"AAAAAAAA-0000-4000-8000-000000000002","runtimeIdentifier":"com.apple.CoreSimulator.SimRuntime.iOS-17-5","version":"17.5","sizeBytes":6800000000,"deletable":true}}"#;

    #[test]
    fn 大小字符串按_1000_进位() {
        assert_eq!(parse_size("1.106GB (2%)"), 1_106_000_000);
        assert_eq!(parse_size("462.7MB"), 462_700_000);
        assert_eq!(parse_size("4.321MB (0%)"), 4_321_000);
        assert_eq!(parse_size("12.5kB"), 12_500);
        assert_eq!(parse_size("0B"), 0);
        assert_eq!(parse_size(""), 0);
    }

    #[test]
    fn 读懂_docker_system_df() {
        let Docker::Ready { images, containers, volumes, build_cache } = parse_docker_df(DF) else { panic!("应该是 Ready") };
        assert_eq!((images.size_bytes, images.reclaimable_bytes, images.count), (42_030_000_000, 1_106_000_000, 30));
        assert_eq!(containers.reclaimable_bytes, 4_321_000);
        assert_eq!(volumes.size_bytes, 1_874_000_000);
        assert_eq!((build_cache.size_bytes, build_cache.reclaimable_bytes), (1_467_000_000, 709_700_000));
    }

    #[test]
    fn 读懂模拟器设备和系统版本() {
        assert_eq!(runtime_label("com.apple.CoreSimulator.SimRuntime.iOS-18-3"), "iOS 18.3");
        assert_eq!(runtime_label("com.apple.CoreSimulator.SimRuntime.watchOS-11-0"), "watchOS 11.0");
        let devices = parse_devices(DEVICES, &|p| (p == Path::new("/tmp/b")).then_some(42));
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].name, "iPhone 17 Pro", "按大小降序");
        assert_eq!((devices[0].available, devices[0].size_bytes, devices[0].last_used_ms, devices[0].os.as_str()), (true, 2_000_000_000, Some(42), "iOS 26.0"));
        assert!(!devices[1].available);
        let runtimes = parse_runtimes(RUNTIMES, &devices);
        let ios26 = runtimes.iter().find(|r| r.version == "26.0").unwrap();
        assert!(ios26.in_use, "iPhone 17 Pro 在用 iOS 26");
        assert_eq!((ios26.size_bytes, ios26.os.as_str()), (7_500_000_000, "iOS 26.0"));
        assert!(!runtimes.iter().find(|r| r.version == "17.5").unwrap().in_use);
        assert!(parse_runtimes("{}", &devices).is_empty(), "本机没有系统版本时的真实输出");
        assert!(parse_runtimes("Total Disk Images: 0 (0.0G)", &devices).is_empty(), "不认识的格式返回空");
        assert!(parse_devices("not json", &|_| None).is_empty());
    }

    #[test]
    fn 合计只算能清理的部分() {
        let devices = parse_devices(DEVICES, &|_| None);
        let r = DevEnvReport { docker: parse_docker_df(DF), simulators: Some(Simulators { runtimes: parse_runtimes(RUNTIMES, &devices), devices }) };
        let docker = 42_030_000_000 + 462_700_000 + 1_467_000_000; // 不含数据卷
        let sims = 18_337_792 + 2_000_000_000 + 7_500_000_000 + 6_800_000_000;
        assert_eq!(total(&r), docker + sims, "不含数据卷");
    }

    #[test]
    fn 命令超时会被杀掉_大量输出不会卡住() {
        let start = Instant::now();
        assert!(run(Path::new("/bin/sleep"), &["5"], Duration::from_millis(200)).is_err());
        assert!(start.elapsed() < Duration::from_secs(2));
        // 200 KB 远超管道缓冲区
        let out = run(Path::new("/bin/dd"), &["if=/dev/zero", "bs=1024", "count=200"], Duration::from_secs(5)).unwrap();
        assert_eq!(out.len(), 200 * 1024);
        assert!(run(Path::new("/bin/ls"), &["/no/such/dir"], Duration::from_secs(5)).is_err(), "退出码不为 0 算失败");
    }

    #[test]
    fn 腾出的空间按前后差值算_读不到时用可回收估计() {
        use crate::remove::Target;
        let devices = parse_devices(DEVICES, &|_| None);
        let before = DevEnvReport { docker: parse_docker_df(DF), simulators: Some(Simulators { devices, runtimes: vec![] }) };
        // 正常：构建缓存从 1.467 GB 降到 0.757 GB
        let mut after = before.clone();
        if let Docker::Ready { build_cache, .. } = &mut after.docker {
            build_cache.size_bytes = 757_000_000;
        }
        assert_eq!(freed(&before, &after, &[Target::DockerBuildCache]), (710_000_000, false));
        // 清理后 Docker 没响应：不能把 Docker 的全部大小当成腾出的，用所选目标的「可回收」
        let after = DevEnvReport { docker: Docker::NotRunning, ..before.clone() };
        assert_eq!(freed(&before, &after, &[Target::DockerBuildCache]), (709_700_000, true));
        // 清理后模拟器列表读不到：用所选设备清理前的大小
        let after = DevEnvReport { simulators: None, ..before.clone() };
        assert_eq!(freed(&before, &after, &[Target::SimUnavailable]), (18_337_792, true));
    }

    #[test]
    fn 没用过模拟器就不调_xcrun() {
        // 没装开发者工具时 /usr/bin/xcrun 会弹系统的安装框，所以先看有没有模拟器目录
        let t = TempTree::new();
        assert_eq!(simulators(&t.0), None);
    }

    #[test]
    fn 空间去向里能跳过来的目录() {
        let home = Path::new("/Users/me");
        assert_eq!(cleanable_dirs(home), [home.join("Library/Containers/com.docker.docker"), home.join("Library/Developer/CoreSimulator")]);
    }

    /// 本机只读跑一遍：`cargo test --lib 在本机读一遍开发环境 -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn 在本机读一遍开发环境() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        eprintln!("{:#?}", report(&home));
    }
}
