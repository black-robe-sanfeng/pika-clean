//! 保养 Mac 页的体检：电池、硬盘的寿命相关指标，以及耗电、写盘最多的应用。全部只读，不改任何设置。

use crate::model::{BatteryCare, CareReport, DiskCare};
use std::process::Command;

/// 耗电、写盘各列几个应用
const HOGS: usize = 5;

pub fn report() -> CareReport {
    let disk = crate::metrics::startup_disk(crate::metrics::purgeable());
    let (energy, writes) = crate::metrics::hogs(HOGS);
    CareReport {
        battery: battery(),
        disk: DiskCare { used_bytes: disk.used_bytes, total_bytes: disk.total_bytes, smart: crate::sensors::ssd_smart() },
        energy,
        writes,
    }
}

/// 退出耗电或写盘榜上的应用。名字必须还在后端重新算出的榜上且是 `.app`，进程号由后端查，不信前端传的。
pub fn quit(name: &str) -> Result<(), String> {
    let (energy, writes) = crate::metrics::hogs(HOGS);
    let pid = energy.iter().chain(&writes).find(|h| h.app && h.name == name).map(|h| h.pid).ok_or(crate::i18n::t("进程已不在列表里"))?;
    crate::system::quit(pid)
}

fn battery() -> Option<BatteryCare> {
    let out = Command::new("/usr/sbin/ioreg").args(["-rn", "AppleSmartBattery", "-d1"]).output().ok()?;
    let mut b = parse_ioreg(&String::from_utf8_lossy(&out.stdout))?;
    // 健康度用系统报告的数，和「系统设置 › 电池」一致；ioreg 里的原始容量比它低几个点
    (b.health_pct, _) = crate::sensors::battery_health();
    Some(b)
}

/// 解析 `ioreg -rn AppleSmartBattery` 的顶层键。没有电池（输出为空）返回 None。
fn parse_ioreg(out: &str) -> Option<BatteryCare> {
    let field = |key: &str| {
        let prefix = format!("\"{key}\" = ");
        out.lines().find_map(|l| l.trim().strip_prefix(&prefix).map(str::to_owned))
    };
    let num = |key: &str| field(key).and_then(|v| v.parse::<u32>().ok());
    let yes = |key: &str| field(key).is_some_and(|v| v == "Yes");
    let cycles = num("CycleCount")?;
    // CurrentCapacity / MaxCapacity：Apple 芯片上是百分比 / 100，Intel 上都是毫安时
    let pct = num("CurrentCapacity").zip(num("MaxCapacity")).filter(|(_, m)| *m > 0).map(|(c, m)| (c * 100 / m).min(100) as u8);
    Some(BatteryCare {
        health_pct: None,
        cycles: Some(cycles),
        design_cycles: num("DesignCycleCount9C"),
        design_mah: num("DesignCapacity"),
        max_mah: num("AppleRawMaxCapacity"),
        // 单位是百分之一摄氏度
        temp_c: num("Temperature").map(|t| t as f32 / 100.0),
        pct,
        plugged: yes("ExternalConnected"),
        charging: yes("IsCharging"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 解析电池注册表() {
        let out = "+-o AppleSmartBattery\n  {\n      \"CurrentCapacity\" = 80\n      \"ExternalConnected\" = Yes\n      \"DesignCycleCount9C\" = 1000\n      \"MaxCapacity\" = 100\n      \"Temperature\" = 3074\n      \"IsCharging\" = No\n      \"DesignCapacity\" = 8694\n      \"CycleCount\" = 220\n      \"AppleRawMaxCapacity\" = 7104\n  }\n";
        let b = parse_ioreg(out).unwrap();
        assert_eq!(
            b,
            BatteryCare { health_pct: None, cycles: Some(220), design_cycles: Some(1000), design_mah: Some(8694), max_mah: Some(7104), temp_c: Some(30.74), pct: Some(80), plugged: true, charging: false }
        );
        assert_eq!(parse_ioreg(""), None);
    }

    #[test]
    fn 本机体检能跑通() {
        let r = report();
        println!("{r:?}");
        assert!(r.disk.total_bytes > 0);
        assert!(r.writes.len() <= HOGS && r.energy.len() <= HOGS);
        // 测试进程自己至少写过点东西不一定，但列出来的都按从大到小排
        assert!(r.writes.windows(2).all(|w| w[0].value >= w[1].value));
    }
}
