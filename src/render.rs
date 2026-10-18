//! GDI+ 抗锯齿渲染（分层窗口 + 逐像素 alpha）。
//!
//! 和纯 GDI 的区别：分层窗口用 `UpdateLayeredWindow(ULW_ALPHA)` 提交一张带 alpha
//! 的位图，圆角和文字都是逐像素抗锯齿的；而 `SetWindowRgn` 是 1-bit 区域，边缘必然
//! 有锯齿（毛刺）。
//!
//! 两个 GDI+ 的坑（沿用 deepseek_balance 项目的经验）：
//! 1) 测量和绘制必须用同一个 GenericTypographic StringFormat，否则默认的 StringFormat
//!    会在文字左右各留 1/6 em 排版留白，把整行字往右推。
//! 2) 垂直居中不用 StringAlignmentCenter：GDI+ 按整行盒（含行距）算，要用
//!    cell ascent + descent 自己定位。

use windows::core::PCWSTR;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
};
use windows::Win32::Graphics::GdiPlus::Ok as GDIP_OK;
use windows::Win32::Graphics::GdiPlus::*;

/// GDI+ 像素格式：32bpp 预乘 alpha，正好是 UpdateLayeredWindow(ULW_ALPHA) 要的格式
const PIXEL_FORMAT_32BPP_PARGB: i32 = 0x000E_200B;

pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ------------------------------------------------------------------ RAII 守卫
// GDI+ flat API 全是裸指针，用守卫保证「画完即还」，不泄漏。

pub struct PathGuard(pub *mut GpPath);
impl PathGuard {
    pub fn new() -> Option<Self> {
        let mut path: *mut GpPath = std::ptr::null_mut();
        (unsafe { GdipCreatePath(FillModeAlternate, &mut path) } == GDIP_OK && !path.is_null())
            .then_some(Self(path))
    }
}
impl Drop for PathGuard {
    fn drop(&mut self) {
        unsafe { GdipDeletePath(self.0) };
    }
}

pub struct BrushGuard(pub *mut GpBrush);
impl BrushGuard {
    pub fn solid(color: u32) -> Option<Self> {
        let mut brush: *mut GpSolidFill = std::ptr::null_mut();
        (unsafe { GdipCreateSolidFill(color, &mut brush) } == GDIP_OK && !brush.is_null())
            .then_some(Self(brush as *mut GpBrush))
    }
    /// 垂直线性渐变（top → bottom）
    pub fn gradient(top: f32, bottom: f32, x: f32, color_top: u32, color_bottom: u32) -> Option<Self> {
        let point1 = PointF { X: x, Y: top };
        let point2 = PointF { X: x, Y: bottom };
        let mut brush: *mut GpLineGradient = std::ptr::null_mut();
        (unsafe {
            GdipCreateLineBrush(&point1, &point2, color_top, color_bottom, WrapModeTile, &mut brush)
        } == GDIP_OK
            && !brush.is_null())
        .then_some(Self(brush as *mut GpBrush))
    }
}
impl Drop for BrushGuard {
    fn drop(&mut self) {
        unsafe { GdipDeleteBrush(self.0) };
    }
}

pub struct FontGuard(pub *mut GpFont);
impl FontGuard {
    pub fn new(family: *const GpFontFamily, pixels: f32) -> Option<Self> {
        let mut font: *mut GpFont = std::ptr::null_mut();
        (unsafe { GdipCreateFont(family, pixels, FontStyleBold.0, UnitPixel, &mut font) } == GDIP_OK
            && !font.is_null())
        .then_some(Self(font))
    }
    pub fn size(&self) -> f32 {
        let mut size = 0.0f32;
        unsafe { GdipGetFontSize(self.0, &mut size) };
        size
    }
}
impl Drop for FontGuard {
    fn drop(&mut self) {
        unsafe { GdipDeleteFont(self.0) };
    }
}

struct StringFormatGuard(*mut GpStringFormat);
impl StringFormatGuard {
    fn generic_typographic() -> Option<Self> {
        let mut format: *mut GpStringFormat = std::ptr::null_mut();
        (unsafe { GdipStringFormatGetGenericTypographic(&mut format) } == GDIP_OK && !format.is_null())
            .then_some(Self(format))
    }
}
impl Drop for StringFormatGuard {
    fn drop(&mut self) {
        unsafe { GdipDeleteStringFormat(self.0) };
    }
}

// ------------------------------------------------------------------ 分层表面

/// 一张 32bpp 位图 + 绑在它上面的 GDI+ 画布，提交给 UpdateLayeredWindow
pub struct LayeredSurface {
    mem_dc: HDC,
    dib: HBITMAP,
    old_bitmap: HGDIOBJ,
    bitmap: *mut GpBitmap,
    graphics: *mut GpGraphics,
    width: i32,
    height: i32,
}

impl LayeredSurface {
    pub fn new() -> Self {
        Self {
            mem_dc: HDC::default(),
            dib: HBITMAP::default(),
            old_bitmap: HGDIOBJ::default(),
            bitmap: std::ptr::null_mut(),
            graphics: std::ptr::null_mut(),
            width: 0,
            height: 0,
        }
    }

    pub fn release(&mut self) {
        // 顺序要紧：GDI+ 对象先没，再还 GDI 位图，最后删 DC
        unsafe {
            if !self.graphics.is_null() {
                GdipDeleteGraphics(self.graphics);
                self.graphics = std::ptr::null_mut();
            }
            if !self.bitmap.is_null() {
                GdipDisposeImage(self.bitmap as *mut GpImage);
                self.bitmap = std::ptr::null_mut();
            }
            if !self.mem_dc.is_invalid() && !self.old_bitmap.is_invalid() {
                SelectObject(self.mem_dc, self.old_bitmap);
            }
            self.old_bitmap = HGDIOBJ::default();
            if !self.dib.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(self.dib.0));
                self.dib = HBITMAP::default();
            }
            if !self.mem_dc.is_invalid() {
                let _ = DeleteDC(self.mem_dc);
                self.mem_dc = HDC::default();
            }
        }
        self.width = 0;
        self.height = 0;
    }

    pub fn ensure(&mut self, width: i32, height: i32) -> bool {
        if width <= 0 || height <= 0 {
            return false;
        }
        if !self.graphics.is_null() && width == self.width && height == self.height {
            return true;
        }

        self.release();

        let mut info = BITMAPINFO::default();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height, // 负数 = 自上而下，行序和 GDI+ 一致
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };

        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let dib = unsafe {
            CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0).unwrap_or_default()
        };
        if dib.is_invalid() || bits.is_null() {
            if !dib.is_invalid() {
                unsafe {
                    let _ = DeleteObject(HGDIOBJ(dib.0));
                }
            }
            return false;
        }

        let mem_dc = unsafe { CreateCompatibleDC(None) };
        if mem_dc.is_invalid() {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(dib.0));
            }
            return false;
        }

        self.mem_dc = mem_dc;
        self.dib = dib;
        self.old_bitmap = unsafe { SelectObject(mem_dc, HGDIOBJ(dib.0)) };

        let mut bitmap: *mut GpBitmap = std::ptr::null_mut();
        let status = unsafe {
            GdipCreateBitmapFromScan0(
                width,
                height,
                width * 4,
                PIXEL_FORMAT_32BPP_PARGB,
                Some(bits as *const u8),
                &mut bitmap,
            )
        };
        if status != GDIP_OK || bitmap.is_null() {
            self.release();
            return false;
        }
        self.bitmap = bitmap;

        let mut graphics: *mut GpGraphics = std::ptr::null_mut();
        let status = unsafe { GdipGetImageGraphicsContext(bitmap as *mut GpImage, &mut graphics) };
        if status != GDIP_OK || graphics.is_null() {
            self.release();
            return false;
        }
        self.graphics = graphics;

        self.width = width;
        self.height = height;
        true
    }

    pub fn graphics(&self) -> *mut GpGraphics {
        self.graphics
    }

    pub fn dc(&self) -> HDC {
        self.mem_dc
    }
}

impl Drop for LayeredSurface {
    fn drop(&mut self) {
        self.release();
    }
}

// ------------------------------------------------------------------ 字体

/// 界面字体族：按回退链找第一个系统里真实存在的。
///
/// family 是「故意泄漏」的：静态变量的析构发生在 main 返回之后，那时 GdiplusShutdown
/// 早就跑过了，再去析构 GDI+ 对象会崩，所以创建完就不管了。
pub fn ui_font_family() -> *mut GpFontFamily {
    use std::sync::OnceLock;
    // GDI+ 对象只在 UI 线程使用，裸指针包一层宣告 Send/Sync 才能放进 OnceLock
    struct SendPtr<T>(T);
    unsafe impl<T> Send for SendPtr<T> {}
    unsafe impl<T> Sync for SendPtr<T> {}
    static FAMILY: OnceLock<SendPtr<*mut GpFontFamily>> = OnceLock::new();
    FAMILY
        .get_or_init(|| {
            for name in ["Microsoft YaHei UI", "Microsoft YaHei", "Segoe UI", "Arial"] {
                let wide = to_wide(name);
                let mut family: *mut GpFontFamily = std::ptr::null_mut();
                let status = unsafe {
                    GdipCreateFontFamilyFromName(PCWSTR(wide.as_ptr()), std::ptr::null_mut(), &mut family)
                };
                if status == GDIP_OK && !family.is_null() {
                    return SendPtr(family);
                }
            }
            SendPtr(std::ptr::null_mut())
        })
        .0
}

// ------------------------------------------------------------------ 图形助手

/// 圆角矩形路径。GDI+ 的角度：0° 在三点钟方向，正方向顺时针，四个角各扫 90°
pub fn add_rounded_rect(path: *mut GpPath, x: f32, y: f32, width: f32, height: f32, radius: f32) {
    if width <= 0.0 || height <= 0.0 {
        return;
    }
    let radius = radius.clamp(0.0, width.min(height) / 2.0);
    if radius <= 0.5 {
        unsafe { GdipAddPathRectangle(path, x, y, width, height) };
        return;
    }
    let d = radius * 2.0;
    unsafe {
        GdipAddPathArc(path, x, y, d, d, 180.0, 90.0); // 左上
        GdipAddPathArc(path, x + width - d, y, d, d, 270.0, 90.0); // 右上
        GdipAddPathArc(path, x + width - d, y + height - d, d, d, 0.0, 90.0); // 右下
        GdipAddPathArc(path, x, y + height - d, d, d, 90.0, 90.0); // 左下
        GdipClosePathFigure(path);
    }
}

pub fn fill_path(graphics: *mut GpGraphics, brush: &BrushGuard, path: &PathGuard) {
    unsafe { GdipFillPath(graphics, brush.0, path.0) };
}

// ------------------------------------------------------------------ 文字

/// 文字宽度（像素）。GenericTypographic 不会在两边塞 1/6 em 的排版留白
pub fn measure_text_width(graphics: *mut GpGraphics, font: *const GpFont, text: &str) -> f32 {
    if text.is_empty() {
        return 0.0;
    }
    let Some(format) = StringFormatGuard::generic_typographic() else {
        return 0.0;
    };
    let wide = to_wide(text);
    let layout = RectF {
        X: 0.0,
        Y: 0.0,
        Width: 1.0e5,
        Height: 1.0e5,
    };
    let mut bounds = RectF::default();
    unsafe {
        GdipMeasureString(
            graphics,
            PCWSTR(wide.as_ptr()),
            (wide.len() - 1) as i32,
            font,
            &layout,
            format.0,
            &mut bounds,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
    }
    bounds.Width
}

#[derive(Clone, Copy, Default)]
pub struct TextMetrics {
    pub ascent: f32,
    pub descent: f32,
}

impl TextMetrics {
    pub fn line_space(&self) -> f32 {
        self.ascent + self.descent
    }
}

/// cell 度量按 em 等比换算成像素（字号是以像素给的，UnitPixel）
pub fn text_metrics(font: &FontGuard, family: *const GpFontFamily) -> TextMetrics {
    let pixels = font.size();
    let mut em_height = 0u16;
    let mut ascent = 0u16;
    let mut descent = 0u16;
    unsafe {
        GdipGetEmHeight(family, FontStyleBold.0, &mut em_height);
        GdipGetCellAscent(family, FontStyleBold.0, &mut ascent);
        GdipGetCellDescent(family, FontStyleBold.0, &mut descent);
    }
    if em_height == 0 {
        return TextMetrics {
            ascent: pixels * 0.8, // 兜底，正常不会走到
            descent: pixels * 0.2,
        };
    }
    let em = em_height as f32;
    TextMetrics {
        ascent: ascent as f32 * pixels / em,
        descent: descent as f32 * pixels / em,
    }
}

/// 在 (center_x, center_y) 处画一行水平和垂直都居中的文字
pub fn draw_centered_text(
    graphics: *mut GpGraphics,
    text: &str,
    font: *const GpFont,
    metrics: &TextMetrics,
    color: u32,
    center_x: f32,
    center_y: f32,
) {
    if text.is_empty() {
        return;
    }
    let Some(format) = StringFormatGuard::generic_typographic() else {
        return;
    };
    let Some(brush) = BrushGuard::solid(color) else {
        return;
    };
    let width = measure_text_width(graphics, font, text);
    unsafe {
        GdipSetStringFormatAlign(format.0, StringAlignmentNear);
        GdipSetStringFormatLineAlign(format.0, StringAlignmentNear);
    }
    let layout = RectF {
        X: center_x - width / 2.0,
        Y: center_y - metrics.line_space() / 2.0,
        Width: width + 1.0,
        Height: metrics.line_space() + 1.0,
    };
    let wide = to_wide(text);
    unsafe {
        GdipDrawString(
            graphics,
            PCWSTR(wide.as_ptr()),
            (wide.len() - 1) as i32,
            font,
            &layout,
            format.0,
            brush.0,
        );
    }
}
