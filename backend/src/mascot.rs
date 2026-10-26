//! 菜单栏图标：应用扫帚与闪光点的单色版。
//! 图是启动时算出来的单色模板图（只有透明度有意义），系统会按深浅色自动着色。
//! 开发版（`make dev`）在外面多画一个圆圈，和装好的正式版区分开；程序坞图标同理，见 Makefile。

/// 菜单栏模板图的像素边长，显示尺寸由 tray.rs 设置。
pub const SIZE: u32 = 36;

type P = (f32, f32);

fn dist(a: P, b: P) -> f32 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

/// `ring` 为真时扫帚缩小一些，外面套一个圆圈。
pub fn icon(ring: bool) -> Vec<u8> {
    let (sin, cos) = 34.0_f32.to_radians().sin_cos();
    let zoom = if ring { 0.88 } else { 1.3 };
    let inside = |p: P| {
        if ring && (10.0..=11.3).contains(&dist(p, (12.0, 12.0))) {
            return true;
        }
        let p = (12.0 + (p.0 - 12.0) / zoom, 12.0 + (p.1 - 12.0) / zoom);
        let (dx, dy) = (p.0 - 12.0, p.1 - 12.0);
        let (x, y) = (12.0 + cos * dx + sin * dy, 12.0 - sin * dx + cos * dy);
        let handle = dist((x, y), (12.0, y.clamp(5.8, 12.9))) < 1.3;
        let collar = dist((x, y), (x.clamp(10.4, 13.6), 13.25)) < 0.9;
        let bristles = (13.6..=18.8).contains(&y) && (x - 12.0).abs() < 1.8 + (y - 13.6) * 0.32;
        let grooves = y > 15.0
            && y < 18.3
            && ((x - 11.1 + (y - 15.0) * 0.12).abs() < 0.18
                || (x - 12.9 - (y - 15.0) * 0.12).abs() < 0.18);
        let sparkle = |center: P, radius: f32| {
            let (sx, sy) = ((p.0 - center.0).abs(), (p.1 - center.1).abs());
            sx.sqrt() + sy.sqrt() < radius.sqrt()
        };
        handle
            || collar
            || (bristles && !grooves)
            || sparkle((18.0, 8.2), 2.5)
            || sparkle((18.6, 13.0), 1.4)
    };

    let scale = SIZE as f32 / 24.0;
    // 每个像素取 4×4 个点，落在图形里的比例就是它的不透明度
    const N: u32 = 4;
    let mut out = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let hits = (0..N * N)
                .filter(|k| {
                    inside((
                        (x as f32 + ((k % N) as f32 + 0.5) / N as f32) / scale,
                        (y as f32 + ((k / N) as f32 + 0.5) / N as f32) / scale,
                    ))
                })
                .count();
            out.extend_from_slice(&[0, 0, 0, (hits * 255 / (N * N) as usize) as u8]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha(f: &[u8], ux: f32, uy: f32) -> u8 {
        let s = SIZE as f32 / 24.0;
        let (x, y) = ((ux * s) as u32, (uy * s) as u32);
        f[((y * SIZE + x) * 4 + 3) as usize]
    }

    #[test]
    fn 图标形状正确() {
        let f = icon(false);
        assert_eq!(f.len(), (SIZE * SIZE * 4) as usize);
        assert_eq!(alpha(&f, 14.0, 8.0), 255, "扫帚手柄是实心的");
        assert_eq!(alpha(&f, 9.0, 16.0), 255, "扫帚头是实心的");
        assert!(alpha(&f, 19.8, 6.8) > 0, "扫帚旁有闪光点");
        assert_eq!(alpha(&f, 5.0, 6.0), 0, "不再绘制旧圆徽");
        assert_eq!(alpha(&f, 1.0, 1.0), 0, "角落是透明的");
        assert!(
            f.chunks(4).all(|px| px[..3] == [0, 0, 0]),
            "模板图只用透明度"
        );
    }

    #[test]
    fn 图案放大且边缘不裁切() {
        let f = icon(false);
        let occupied: Vec<_> = f
            .chunks_exact(4)
            .enumerate()
            .filter(|(_, px)| px[3] > 0)
            .map(|(i, _)| (i % SIZE as usize, i / SIZE as usize))
            .collect();
        let min_x = occupied.iter().map(|p| p.0).min().unwrap();
        let max_x = occupied.iter().map(|p| p.0).max().unwrap();
        let min_y = occupied.iter().map(|p| p.1).min().unwrap();
        let max_y = occupied.iter().map(|p| p.1).max().unwrap();
        assert!(
            max_x - min_x >= 26 && max_y - min_y >= 26,
            "图案应填满大部分画布"
        );
        assert!(
            min_x > 0 && min_y > 0 && max_x < SIZE as usize - 1 && max_y < SIZE as usize - 1,
            "四边保留透明留白"
        );
    }

    #[test]
    fn 开发版多一个圆圈且扫帚不碰到圆圈() {
        let f = icon(true);
        assert_eq!(alpha(&f, 12.0, 1.0), 255, "顶上是圆圈");
        assert_eq!(alpha(&f, 1.0, 12.0), 255, "左边是圆圈");
        assert_eq!(alpha(&f, 12.0, 2.5), 0, "圆圈内侧留白");
        assert_eq!(alpha(&f, 0.3, 0.3), 0, "角落仍透明");
        assert!((0..SIZE).all(|i| [(i, 0), (0, i), (i, SIZE - 1), (SIZE - 1, i)].iter().all(|&(x, y)| f[((y * SIZE + x) * 4 + 3) as usize] == 0)), "四边留一像素透明");
        // 圆圈以内、离圆圈不到一格的地方都是空的：扫帚没顶到圆圈
        let s = SIZE as f32 / 24.0;
        for (i, px) in f.chunks_exact(4).enumerate() {
            let (x, y) = (((i as u32 % SIZE) as f32 + 0.5) / s, ((i as u32 / SIZE) as f32 + 0.5) / s);
            let r = dist((x, y), (12.0, 12.0));
            if (9.0..9.6).contains(&r) {
                assert_eq!(px[3], 0, "扫帚碰到了圆圈：({x}, {y})");
            }
        }
    }
}
