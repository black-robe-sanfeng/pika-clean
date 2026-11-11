//! 应用图标：用系统的 `NSWorkspace` 取，和「访达」里看到的一样（只用 Assets.car 存图标的新应用也取得到）。

use std::path::{Path, PathBuf};

use objc2::AllocAnyThread;
use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSWorkspace};
use objc2_foundation::{NSDictionary, NSPoint, NSRect, NSSize, NSString};

/// 界面上最大的图标是 38 点，取 64 点，小的由浏览器缩。系统按屏幕倍数出像素：两倍屏是 128 像素
const PT: f64 = 64.0;

/// 只认真实存在的 `.app` 文件夹（解析掉符号链接和 `..` 之后）。图标只读，但前端传来的路径一律在这里校验。
pub fn check(path: &str) -> Option<PathBuf> {
    let p = Path::new(path).canonicalize().ok()?;
    (p.extension().is_some_and(|e| e == "app") && p.is_dir()).then_some(p)
}

/// `.app` 的图标，PNG 格式。
pub fn png(app: &Path) -> Option<Vec<u8>> {
    objc2::rc::autoreleasepool(|_| {
        let image = NSWorkspace::sharedWorkspace().iconForFile(&NSString::from_str(app.to_str()?));
        let mut rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(PT, PT));
        // SAFETY: rect 是本函数的局部变量；不传绘图上下文和提示，由系统按尺寸挑最合适的那张图
        let cg = unsafe { image.CGImageForProposedRect_context_hints(&mut rect, None, None) }?;
        let rep = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), &cg);
        // SAFETY: 空字典表示用默认的 PNG 参数
        let data = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) }?;
        Some(data.to_vec())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 只认存在的应用包() {
        assert!(check("/System/Applications/Calculator.app").is_some());
        assert!(check("/System/Applications/Calculator.app/Contents").is_none());
        assert!(check("/System/Applications/../Applications/Calculator.app").is_some());
        assert!(check("/etc/hosts").is_none());
        assert!(check("/nope/Fake.app").is_none());
        assert!(check("relative.app").is_none());
    }

    #[test]
    fn 取到的是_png_图标() {
        let data = png(&check("/System/Applications/Calculator.app").unwrap()).unwrap();
        assert_eq!(&data[1..4], b"PNG");
        // 宽在 PNG 文件头的第 16 到 19 字节
        let w = u32::from_be_bytes(data[16..20].try_into().unwrap());
        assert!([64, 128, 192].contains(&w), "宽 {w}");
    }
}
