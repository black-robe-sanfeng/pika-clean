//! 相似照片：连拍、同一张照片导出了好几份大小不同的版本。
//! 用系统自带的 `sips` 把每张照片缩成 9×8 的小图，算差值哈希（dHash）和平均颜色，两样都接近的算一组；
//! 每组留最大的一张（通常分辨率最高），其余列出来。不加图片解码依赖，HEIC 也能读。

use crate::scan::{keep_atime, last_used, walk_personal, Stop};
use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// 在这几个位置找（相对主目录）。「照片」图库是包，不进去。
const DIRS: [&str; 4] = ["Pictures", "Desktop", "Downloads", "Documents"];
/// 只看相机拍的格式。PNG 多是截图、导出的设计稿和文档页面，版式相近的很多，容易误认。
const EXTS: [&str; 4] = ["jpg", "jpeg", "heic", "heif"];

/// 系统索引里记着拍摄设备的照片，也就是相机、手机拍的。扫描件、软件截图、导出图没有这项，不看：它们版式相近，容易误认。
/// 没开 Spotlight 索引时一张都没有。
fn camera_photos(home: &Path) -> std::collections::HashSet<PathBuf> {
    DIRS.iter()
        .flat_map(|d| {
            let dir = home.join(d).to_string_lossy().into_owned();
            let out = crate::devenv::run(Path::new("/usr/bin/mdfind"), &["-onlyin", &dir, "kMDItemAcquisitionModel == \"*\""], std::time::Duration::from_secs(5)).unwrap_or_default();
            out.lines().map(PathBuf::from).collect::<Vec<_>>()
        })
        .collect()
}
/// 小图标、表情不看。
const MIN_BYTES: u64 = 100_000;
/// 最多看这么多张，超过就标「至少」。
// ponytail: 两两比较是 O(n²)，3000 张约 450 万次比较，不到一秒；照片更多时再按拍摄时间分桶
const MAX_PHOTOS: usize = 3000;
/// 一次交给 `sips` 的张数。
const BATCH: usize = 200;
/// 缩成这么大的小图来比：宽比高多 1，每行能比 W-1 次，一共 64 位。
// ponytail: 再大的小图在平滑区域（天空、白墙）哈希位会乱跳，同一张图缩放后反而认不出来
const W: usize = 9;
const H: usize = 8;
/// 64 位里最多差这么多位算相似。
const MAX_DISTANCE: u32 = 5;
/// 整体颜色（红绿蓝各自的平均值）最多差这么多才算相似：结构相近、配色不同的图（不同主题的终端截图）不算。
const MAX_COLOR_DIFF: i32 = 12;

/// 一张照片的指纹：差值哈希加平均颜色。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Print {
    hash: u64,
    color: [i32; 3],
}

impl Print {
    fn distance(&self, o: &Print) -> u32 {
        (self.hash ^ o.hash).count_ones()
    }
    /// 几乎纯色的图（白底截图、空白页）哈希接近全 0，彼此都「相似」，不参与比较。
    fn too_plain(&self) -> bool {
        !(8..=56).contains(&self.hash.count_ones())
    }
    fn similar(&self, o: &Print) -> bool {
        self.distance(o) <= MAX_DISTANCE && self.color.iter().zip(o.color).all(|(a, b)| (a - b).abs() <= MAX_COLOR_DIFF)
    }
}

/// 读 `sips` 输出的 W×H BMP（24 或 32 位，行可能自下而上），算指纹：每行相邻两点比亮度，左边亮记 1。
pub(crate) fn print_of(d: &[u8]) -> Option<Print> {
    let u32_at = |i: usize| Some(u32::from_le_bytes(d.get(i..i + 4)?.try_into().ok()?));
    let (offset, width, height) = (u32_at(10)? as usize, u32_at(18)? as i32, u32_at(22)? as i32);
    let bpp = u16::from_le_bytes(d.get(28..30)?.try_into().ok()?) as usize / 8;
    if d.get(0..2)? != b"BM" || width != W as i32 || height.unsigned_abs() as usize != H || bpp < 3 {
        return None;
    }
    let stride = (W * bpp).div_ceil(4) * 4;
    let (mut hash, mut sum) = (0u64, [0i32; 3]);
    for y in 0..H {
        let row = offset + if height < 0 { y } else { H - 1 - y } * stride;
        let mut left = None;
        for x in 0..W {
            // BMP 是蓝绿红的顺序
            let p = d.get(row + x * bpp..row + x * bpp + 3)?;
            let (r, g, b) = (i32::from(p[2]), i32::from(p[1]), i32::from(p[0]));
            sum = [sum[0] + r, sum[1] + g, sum[2] + b];
            let lum = r * 299 + g * 587 + b * 114;
            if let Some(l) = left {
                hash = (hash << 1) | u64::from(l > lum);
            }
            left = Some(lum);
        }
    }
    Some(Print { hash, color: sum.map(|s| s / (W * H) as i32) })
}

/// 把照片分组：相似的连成一组。返回每组的下标，只留至少两张的组。
pub(crate) fn groups(prints: &[Print]) -> Vec<Vec<usize>> {
    let mut parent: Vec<usize> = (0..prints.len()).collect();
    fn root(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    for i in 0..prints.len() {
        if prints[i].too_plain() {
            continue;
        }
        for j in i + 1..prints.len() {
            if !prints[j].too_plain() && prints[i].similar(&prints[j]) {
                let (a, b) = (root(&mut parent, i), root(&mut parent, j));
                parent[a] = b;
            }
        }
    }
    let mut by: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..prints.len() {
        let r = root(&mut parent, i);
        by.entry(r).or_default().push(i);
    }
    by.into_values().filter(|g| g.len() > 1).collect()
}

/// 用 `sips` 给一批照片算指纹。输出文件按原文件名（去掉扩展名）命名，所以一批里的文件名不能重复。
fn print_batch(files: &[&PathBuf], out: &Path) -> Vec<Option<Print>> {
    let _ = std::fs::remove_dir_all(out);
    let _ = std::fs::create_dir_all(out);
    let mut cmd = std::process::Command::new("/usr/bin/sips");
    cmd.args(["-s", "format", "bmp", "-z", &H.to_string(), &W.to_string()]).args(files).arg("--out").arg(out);
    let _ = cmd.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
    files
        .iter()
        .map(|f| {
            let bmp = out.join(f.file_stem()?).with_extension("bmp");
            print_of(&std::fs::read(bmp).ok()?)
        })
        .collect()
}

/// 个人文件夹里的相似照片。返回（路径，占用，上次使用），每组最大的那张不在里面。
pub fn find(home: &Path, stop: &Stop) -> (Vec<(PathBuf, u64, Option<SystemTime>)>, bool) {
    let camera = camera_photos(home);
    let mut photos: Vec<(PathBuf, std::fs::Metadata)> = Vec::new();
    let mut done = walk_personal(home, &DIRS, stop, &mut |path, meta| {
        let ext = path.extension().map(|x| x.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        if EXTS.contains(&ext.as_str()) && meta.len() >= MIN_BYTES && photos.len() < MAX_PHOTOS && camera.contains(&path) {
            photos.push((path, meta.clone()));
        }
    });
    done &= photos.len() < MAX_PHOTOS;

    // 分批，同一批里文件名（不含扩展名）不重复
    let mut batches: Vec<(Vec<usize>, std::collections::HashSet<std::ffi::OsString>)> = Vec::new();
    for (i, (p, _)) in photos.iter().enumerate() {
        let stem = p.file_stem().unwrap_or_default().to_os_string();
        match batches.iter_mut().find(|(b, names)| b.len() < BATCH && !names.contains(&stem)) {
            Some((b, names)) => {
                b.push(i);
                names.insert(stem);
            }
            None => batches.push((vec![i], [stem].into())),
        }
    }
    let tmp = std::env::temp_dir().join(format!("pikaclean-similar-{}", std::process::id()));
    let mut prints: Vec<Option<Print>> = vec![None; photos.len()];
    for (batch, _) in &batches {
        if stop.hit() {
            done = false;
            break;
        }
        let files: Vec<&PathBuf> = batch.iter().map(|&i| &photos[i].0).collect();
        for (&i, p) in batch.iter().zip(print_batch(&files, &tmp)) {
            prints[i] = p;
        }
        // sips 读过的照片「上次访问时间」被改成了现在，改回去：旧文件靠它判断多久没用
        for &i in batch {
            if let Ok(f) = std::fs::File::open(&photos[i].0) {
                keep_atime(&f, &photos[i].1);
            }
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);

    let known: Vec<(usize, Print)> = prints.iter().enumerate().filter_map(|(i, p)| Some((i, (*p)?))).collect();
    let mut found = Vec::new();
    for mut g in groups(&known.iter().map(|(_, p)| *p).collect::<Vec<_>>()) {
        // 留最大的那张；一样大留路径排前面的
        g.sort_by_key(|&k| (std::cmp::Reverse(photos[known[k].0].1.len()), photos[known[k].0].0.clone()));
        found.extend(g.into_iter().skip(1).map(|k| {
            let (p, m) = &photos[known[k].0];
            (p.clone(), m.blocks() * 512, last_used(m))
        }));
    }
    (found, done)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 24 位、自上而下的 W×H 图，像素颜色由 `f(x, y)` 给出。
    fn bmp(f: impl Fn(usize, usize) -> [u8; 3]) -> Vec<u8> {
        let stride = (W * 3).div_ceil(4) * 4;
        let mut d = vec![0u8; 54 + stride * H];
        d[0..2].copy_from_slice(b"BM");
        d[10..14].copy_from_slice(&54u32.to_le_bytes());
        d[18..22].copy_from_slice(&(W as i32).to_le_bytes());
        d[22..26].copy_from_slice(&(-(H as i32)).to_le_bytes());
        d[28..30].copy_from_slice(&24u16.to_le_bytes());
        for y in 0..H {
            for x in 0..W {
                let [r, g, b] = f(x, y);
                d[54 + y * stride + x * 3..][..3].copy_from_slice(&[b, g, r]);
            }
        }
        d
    }

    #[test]
    fn 指纹和分组() {
        let pattern = |x: usize, y: usize| ((x * 37 + y * 91) % 200) as u8;
        let a = print_of(&bmp(|x, y| [pattern(x, y); 3])).unwrap();
        assert!(!a.too_plain());
        // 整体亮一点点：还是同一张
        let brighter = print_of(&bmp(|x, y| [pattern(x, y) + 5; 3])).unwrap();
        // 明暗结构一样，但整体偏红：配色不同不算
        let red = print_of(&bmp(|x, y| [pattern(x, y) / 2 + 100, pattern(x, y) / 2, pattern(x, y) / 2])).unwrap();
        // 别的结构
        let other = print_of(&bmp(|x, y| [((x * 13 + y * 7) % 250) as u8; 3])).unwrap();
        let plain = print_of(&bmp(|_, _| [240; 3])).unwrap();
        assert!(plain.too_plain());
        let mut got = groups(&[a, brighter, red, other, plain, plain]);
        got.iter_mut().for_each(|g| g.sort());
        assert_eq!(got, vec![vec![0, 1]]);
        assert!(print_of(&bmp(|_, _| [0; 3])[..60]).is_none(), "数据不全时不出错");
    }

    #[test]
    #[ignore = "要用系统的 sips"]
    fn 真实照片缩图() {
        let dir = crate::scan::testutil::TempTree::new();
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("icons/icon.png");
        // 图标有透明部分，先转成不透明的 JPEG，再做一张缩小的版本
        let a = dir.0.join("a.jpg");
        std::process::Command::new("/usr/bin/sips").args(["-s", "format", "jpeg"]).arg(&src).arg("--out").arg(&a).output().unwrap();
        let b = dir.0.join("b.jpg");
        std::process::Command::new("/usr/bin/sips").args(["-Z", "200"]).arg(&a).arg("--out").arg(&b).output().unwrap();
        let p = print_batch(&[&a, &b], &dir.0.join("out"));
        let (x, y) = (p[0].unwrap(), p[1].unwrap());
        assert!(x.similar(&y), "同一张图缩小后还是相似：{x:?} {y:?}");
    }

    /// 只读探针，平时不跑：在本机真实照片上分组，打印前几组。
    #[test]
    #[ignore = "看本机真实照片"]
    fn 本机的相似照片() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let (found, done) = find(&home, &Stop::for_test(&cancel));
        println!("{} 张，找完了：{done}", found.len());
        for (p, size, _) in found.iter().take(40) {
            println!("{:>7.1} MB  {}", *size as f64 / 1e6, p.strip_prefix(&home).unwrap_or(p).display());
        }
    }
}
