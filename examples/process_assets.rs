//! 一次性资产处理：去水印（左下角擦除/裁底）+ 裁边 + 缩放。
//! 用法：cargo run --example process_assets

use image::{GenericImageView, ImageBuffer, Rgba};

fn erase_rect(img: &mut ImageBuffer<Rgba<u8>, Vec<u8>>, x0: u32, y0: u32, x1: u32, y1: u32) {
    for y in y0..y1.min(img.height()) {
        for x in x0..x1.min(img.width()) {
            img.put_pixel(x, y, Rgba([0, 0, 0, 0]));
        }
    }
}

/// 按 alpha>8 的内容包围盒裁剪（带边距）
fn trim(img: &ImageBuffer<Rgba<u8>, Vec<u8>>, margin: u32) -> image::DynamicImage {
    let (mut x0, mut y0, mut x1, mut y1) = (img.width(), img.height(), 0u32, 0u32);
    for y in 0..img.height() {
        for x in 0..img.width() {
            if img.get_pixel(x, y).0[3] > 8 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    if x1 <= x0 || y1 <= y0 {
        return image::DynamicImage::ImageRgba8(img.clone());
    }
    x0 = x0.saturating_sub(margin);
    y0 = y0.saturating_sub(margin);
    x1 = (x1 + margin).min(img.width() - 1);
    y1 = (y1 + margin).min(img.height() - 1);
    image::DynamicImage::ImageRgba8(img.clone()).crop_imm(x0, y0, x1 - x0 + 1, y1 - y0 + 1)
}

fn main() {
    // logo：擦除左下水印 → 裁边 → 256
    let mut logo = image::open("assets/logo.png").unwrap().to_rgba8();
    erase_rect(&mut logo, 0, 900, 260, 1024);
    let logo = trim(&logo, 6).resize_exact(256, 256, image::imageops::FilterType::Lanczos3);
    logo.save("assets/logo_256.png").unwrap();

    // empty_state：擦除左下水印 → 裁边 → 512
    let mut es = image::open("assets/empty_state.png").unwrap().to_rgba8();
    erase_rect(&mut es, 0, 900, 320, 1024);
    let es = trim(&es, 6).resize(512, 512, image::imageops::FilterType::Lanczos3);
    es.save("assets/empty_state_512.png").unwrap();

    // cover_placeholder：不透明，裁掉底部 56px 水印带 → 640 宽 jpg
    let cp = image::open("assets/cover_placeholder.png").unwrap();
    let (w, _h) = cp.dimensions();
    let cp = cp.crop_imm(0, 0, w, 520).resize(640, 288, image::imageops::FilterType::Lanczos3);
    cp.save("assets/cover_placeholder_640.jpg").unwrap();

    println!("ASSETS_OK");
}
