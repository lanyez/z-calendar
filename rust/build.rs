// 构建时以 icon.png 为源自动生成图标并嵌入 exe：
// 1) 等比居中到正方形画布，缩放多尺寸打包成 icon.ico（≤128 为 BMP 帧，256 为 PNG 帧减小体积）
// 2) 32×32 RGBA 写入 OUT_DIR，托盘图标 include 使用（与 exe 图标同一来源，见 src/tray.rs）
use std::io::Cursor;
use std::path::Path;

const SIZES: &[u32] = &[16, 24, 32, 48, 64, 128, 256];
const TRAY_SIZE: u32 = 32;

fn main() {
    if std::env::var("CARGO_CFG_WINDOWS").is_err() {
        return;
    }
    println!("cargo:rerun-if-changed=icon.png");

    let img = image::open("icon.png").expect("read icon.png").to_rgba8();
    let square = fit_square(&img);
    let frames: Vec<(u32, Vec<u8>)> = SIZES
        .iter()
        .map(|&n| {
            let resized = image::imageops::resize(&square, n, n, image::imageops::FilterType::Lanczos3);
            (n, resized.into_raw())
        })
        .collect();

    std::fs::write("icon.ico", pack_ico(&frames)).expect("write icon.ico");

    let tray = &frames.iter().find(|(n, _)| *n == TRAY_SIZE).unwrap().1;
    let out = std::env::var("OUT_DIR").unwrap();
    std::fs::write(Path::new(&out).join("tray_rgba.bin"), tray).expect("write tray_rgba.bin");

    winresource::WindowsResource::new()
        .set_icon("icon.ico")
        .set("FileDescription", "Z日历")
        .compile()
        .expect("failed to compile windows resource");
}

/// 非正方形图片等比缩放后居中放到正方形透明画布上（contain，不裁剪不变形）
fn fit_square(img: &image::RgbaImage) -> image::RgbaImage {
    let (w, h) = img.dimensions();
    let side = w.max(h);
    let scaled = if w == h {
        img.clone()
    } else if w > h {
        let nh = ((h as f64 * side as f64 / w as f64).round() as u32).max(1);
        image::imageops::resize(img, side, nh, image::imageops::FilterType::Lanczos3)
    } else {
        let nw = ((w as f64 * side as f64 / h as f64).round() as u32).max(1);
        image::imageops::resize(img, nw, side, image::imageops::FilterType::Lanczos3)
    };
    let mut canvas = image::RgbaImage::new(side, side);
    let x = ((side - scaled.dimensions().0) / 2) as i64;
    let y = ((side - scaled.dimensions().1) / 2) as i64;
    image::imageops::overlay(&mut canvas, &scaled, x, y);
    canvas
}

/// ICO 打包：目录项 + 各帧数据
fn pack_ico(frames: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let entries: Vec<(u32, Vec<u8>)> = frames
        .iter()
        .map(|(n, rgba)| {
            let data = if *n >= 256 { png_frame(*n, rgba) } else { bmp_frame(*n, rgba) };
            (*n, data)
        })
        .collect();

    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    let mut offset: u32 = 6 + 16 * entries.len() as u32;
    for (n, data) in &entries {
        out.push(if *n >= 256 { 0 } else { *n as u8 }); // 256 在目录项中记为 0
        out.push(if *n >= 256 { 0 } else { *n as u8 });
        out.push(0); // 调色板色数
        out.push(0); // 保留
        out.extend_from_slice(&1u16.to_le_bytes()); // 颜色平面
        out.extend_from_slice(&32u16.to_le_bytes()); // 位深
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += data.len() as u32;
    }
    for (_, data) in &entries {
        out.extend_from_slice(data);
    }
    out
}

/// BMP 帧：BITMAPINFOHEADER（高度翻倍）+ 自下而上 BGRA + 全 0 AND 掩码（透明度走 alpha 通道）
fn bmp_frame(n: u32, rgba: &[u8]) -> Vec<u8> {
    let mask_row = ((n + 31) / 32) * 4;
    let mut out = Vec::new();
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(n as i32).to_le_bytes());
    out.extend_from_slice(&((n as i32) * 2).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&((n * n * 4 + mask_row * n) as u32).to_le_bytes());
    out.extend_from_slice(&[0u8; 16]); // 分辨率/用色/重要色
    for row in (0..n).rev() {
        for x in 0..n {
            let i = ((row * n + x) * 4) as usize;
            out.push(rgba[i + 2]);
            out.push(rgba[i + 1]);
            out.push(rgba[i]);
            out.push(rgba[i + 3]);
        }
    }
    out.extend(std::iter::repeat(0u8).take((mask_row * n) as usize));
    out
}

/// PNG 帧（仅 256px 使用，体积远小于 BMP 帧）
fn png_frame(n: u32, rgba: &[u8]) -> Vec<u8> {
    let img = image::RgbaImage::from_raw(n, n, rgba.to_vec()).expect("icon frame");
    let mut buf = Cursor::new(Vec::new());
    img.write_with_encoder(image::codecs::png::PngEncoder::new(&mut buf))
        .expect("encode png frame");
    buf.into_inner()
}
