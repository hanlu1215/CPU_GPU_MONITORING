// 以 GUI 子系统运行：不会弹出控制台窗口
#![windows_subsystem = "windows"]

mod cpu;
mod gpu;
mod render;

use std::cell::RefCell;
use windows::core::w;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{
    GetDC, GetDeviceCaps, ReleaseDC, AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION, HDC, LOGPIXELSX,
};
use windows::Win32::Graphics::GdiPlus::{
    GdipGraphicsClear, GdipSetPixelOffsetMode, GdipSetSmoothingMode, GdipSetTextRenderingHint,
    GdiplusShutdown, GdiplusStartup, GdiplusStartupInput, PixelOffsetModeHalf,
    SmoothingModeAntiAlias, TextRenderingHintAntiAlias,
};
use windows::Win32::Graphics::GdiPlus::Ok as GDIP_OK;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    GetCurrentProcess, SetPriorityClass, BELOW_NORMAL_PRIORITY_CLASS,
};
use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
use windows::Win32::UI::WindowsAndMessaging::*;

use render::{BrushGuard, FontGuard, PathGuard};

// ---------- 可调参数 ----------
const TIMER_ID: usize = 1;
const INTERVAL_MS: u32 = 1000; // 刷新间隔
const GPU_REFRESH_EVERY: u32 = 60; // 每 N 次采样重建一次 GPU 查询
const HOT: f64 = 95.0; // 达到该百分比视为满载，变红

// ---------- 布局（设计像素，运行时按系统 DPI 缩放）----------
const OUTER_MARGIN: f32 = 8.0; // 卡片外的透明留白：圆角抗锯齿和描边需要它
const PAD_X: f32 = 14.0; // 左右内边距
const GAP: f32 = 12.0; // 两格之间的间距
const TOP: f32 = 8.0;
const NUM_H: f32 = 38.0; // 大数字行高
const PILL_H: f32 = 22.0; // 标签胶囊高度
const CARD_H: f32 = TOP + NUM_H + 2.0 + PILL_H + 10.0;
const CARD_RADIUS: f32 = 18.0; // 卡片圆角半径
const BORDER: f32 = 1.2; // 描边宽度
const NUM_FONT_PX: f32 = 30.0;
const PILL_FONT_PX: f32 = 13.0;

// ---------- 配色（0xAARRGGBB，与 deepseek_balance 同一套）----------
const C_TOP: u32 = 0xFF2C2F38; // 卡片渐变起始
const C_BOTTOM: u32 = 0xFF1A1C21; // 卡片渐变结束
const C_BORDER: u32 = 0xFF4E5460; // 描边
const C_NUM: u32 = 0xFF5AA9FF; // 数字：蓝
const C_NUM_HOT: u32 = 0xFFFF5F5F; // 满载：红
const C_PILL: u32 = 0xFF16301F; // 胶囊底
const C_PILL_TXT: u32 = 0xFF4EDE9A; // 胶囊字：绿
const C_PILL_HOT: u32 = 0xFF3A1E1E;
const C_PILL_TXT_HOT: u32 = 0xFFFF7070;

const ID_EXIT: usize = 1;

#[derive(Clone, PartialEq)]
struct Cell {
    label: String, // CPU / GPU
    value: String, // 41%
    hot: bool,
}

struct State {
    cpu: cpu::CpuMonitor,
    gpu: Option<gpu::GpuMonitor>,
    tick: u32,
    cells: Vec<Cell>,
    cell_w: f32,
    scale: f32,
    total_w: i32,
    total_h: i32,
    surface: render::LayeredSurface,
}

thread_local! {
    static STATE: RefCell<Option<State>> = RefCell::new(None);
}

fn cell(label: &str, v: f64) -> Cell {
    Cell { label: label.to_string(), value: format!("{:.0}%", v), hot: v >= HOT }
}

fn build_cells(cpu: f64, gpus: Option<&[f64]>) -> Vec<Cell> {
    let mut v = vec![cell("CPU", cpu)];
    match gpus {
        Some(g) if g.len() == 1 => v.push(cell("GPU", g[0])),
        Some(g) if g.len() > 1 => {
            for (i, x) in g.iter().enumerate() {
                v.push(cell(&format!("GPU{}", i), *x));
            }
        }
        _ => v.push(Cell { label: "GPU".into(), value: "N/A".into(), hot: false }),
    }
    v
}

/// 系统 DPI 缩放：96 = 100%
unsafe fn dpi_scale() -> f32 {
    let hdc = GetDC(None);
    let dpi = GetDeviceCaps(hdc, LOGPIXELSX);
    ReleaseDC(None, hdc);
    (dpi as f32 / 96.0).clamp(1.0, 3.0)
}

/// 把分层窗口的内容提交上屏（pptDst 传 None = 位置不动）
unsafe fn present(hwnd: HWND, st: &State) {
    let src = POINT { x: 0, y: 0 };
    let size = SIZE { cx: st.total_w, cy: st.total_h };
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    let _ = UpdateLayeredWindow(
        hwnd,
        HDC::default(),
        None,
        Some(&size),
        st.surface.dc(),
        Some(&src),
        COLORREF(0),
        Some(&blend),
        ULW_ALPHA,
    );
}

/// 画一帧：卡片底 + 每格的大数字和标签胶囊
unsafe fn draw(hwnd: HWND, st: &mut State) {
    let s = st.scale;
    let n = st.cells.len() as i32;
    let card_w = PAD_X * s * 2.0 + n as f32 * st.cell_w + (n - 1).max(0) as f32 * GAP * s;
    let total_w = (card_w + OUTER_MARGIN * s * 2.0).ceil() as i32;
    let total_h = (CARD_H * s + OUTER_MARGIN * s * 2.0).ceil() as i32;

    if total_w != st.total_w || total_h != st.total_h {
        st.total_w = total_w;
        st.total_h = total_h;
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            total_w,
            total_h,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
    if !st.surface.ensure(total_w, total_h) {
        return;
    }

    let g = st.surface.graphics();
    GdipSetSmoothingMode(g, SmoothingModeAntiAlias);
    GdipSetTextRenderingHint(g, TextRenderingHintAntiAlias);
    GdipSetPixelOffsetMode(g, PixelOffsetModeHalf);
    GdipGraphicsClear(g, 0); // 全透明，卡片之外的地方能看见桌面

    let m = OUTER_MARGIN * s;
    let (left, top) = (m, m);
    let (w, h) = (total_w as f32 - 2.0 * m, total_h as f32 - 2.0 * m);
    let radius = CARD_RADIUS * s;
    let center_x = left + w / 2.0;

    // 卡片底：先整块填描边色，再把内缩 BORDER 的区域填渐变
    if let Some(outer) = PathGuard::new() {
        render::add_rounded_rect(outer.0, left, top, w, h, radius);
        if let Some(border_brush) = BrushGuard::solid(C_BORDER) {
            render::fill_path(g, &border_brush, &outer);
        }
        let border = BORDER * s;
        if w - 2.0 * border > 0.0 && h - 2.0 * border > 0.0 {
            if let Some(inner) = PathGuard::new() {
                render::add_rounded_rect(
                    inner.0,
                    left + border,
                    top + border,
                    w - 2.0 * border,
                    h - 2.0 * border,
                    (radius - border).max(0.0),
                );
                if let Some(gradient) = BrushGuard::gradient(top, top + h, center_x, C_TOP, C_BOTTOM) {
                    render::fill_path(g, &gradient, &inner);
                }
            }
        }
    }

    let family = render::ui_font_family();
    if family.is_null() {
        present(hwnd, st);
        return;
    }
    let (Some(big), Some(small)) = (
        FontGuard::new(family, NUM_FONT_PX * s),
        FontGuard::new(family, PILL_FONT_PX * s),
    ) else {
        present(hwnd, st);
        return;
    };
    let big_m = render::text_metrics(&big, family);
    let small_m = render::text_metrics(&small, family);

    for (i, c) in st.cells.iter().enumerate() {
        let x0 = left + PAD_X * s + i as f32 * (st.cell_w + GAP * s);
        let cell_cx = x0 + st.cell_w / 2.0;

        // 大数字
        render::draw_centered_text(
            g,
            &c.value,
            big.0,
            &big_m,
            if c.hot { C_NUM_HOT } else { C_NUM },
            cell_cx,
            top + (TOP + NUM_H / 2.0) * s,
        );

        // 胶囊底色 + 标签
        let pill_top = top + (TOP + NUM_H + 2.0) * s;
        let pill_h = PILL_H * s;
        if let Some(pill) = PathGuard::new() {
            render::add_rounded_rect(pill.0, x0, pill_top, st.cell_w, pill_h, pill_h.min(st.cell_w) / 2.0);
            if let Some(brush) = BrushGuard::solid(if c.hot { C_PILL_HOT } else { C_PILL }) {
                render::fill_path(g, &brush, &pill);
            }
        }
        render::draw_centered_text(
            g,
            &c.label,
            small.0,
            &small_m,
            if c.hot { C_PILL_TXT_HOT } else { C_PILL_TXT },
            cell_cx,
            pill_top + pill_h / 2.0,
        );
    }

    present(hwnd, st);
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_TIMER if wparam.0 == TIMER_ID => {
                STATE.with(|s| {
                    if let Some(st) = s.borrow_mut().as_mut() {
                        st.tick += 1;
                        if st.tick % GPU_REFRESH_EVERY == 0 {
                            st.gpu = gpu::GpuMonitor::new();
                        }
                        let c = st.cpu.sample();
                        let g = st.gpu.as_mut().map(|g| g.sample());
                        let cells = build_cells(c, g.as_deref());
                        // 显示内容没变化就不重绘
                        if cells != st.cells {
                            st.cells = cells;
                            draw(hwnd, st);
                        }
                    }
                });
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            // 按住左键拖动窗口
            WM_LBUTTONDOWN => {
                let _ = ReleaseCapture();
                SendMessageW(hwnd, WM_NCLBUTTONDOWN, WPARAM(HTCAPTION as usize), LPARAM(0));
                LRESULT(0)
            }
            // 右键菜单：退出
            WM_RBUTTONUP => {
                if let Ok(menu) = CreatePopupMenu() {
                    let _ = AppendMenuW(menu, MF_STRING, ID_EXIT, w!("退出"));
                    let mut pt = POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    let _ = SetForegroundWindow(hwnd);
                    let cmd = TrackPopupMenu(
                        menu,
                        TPM_RETURNCMD | TPM_RIGHTBUTTON,
                        pt.x,
                        pt.y,
                        0,
                        hwnd,
                        None,
                    );
                    let _ = DestroyMenu(menu);
                    if cmd.0 as usize == ID_EXIT {
                        let _ = DestroyWindow(hwnd);
                    }
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                let _ = KillTimer(hwnd, TIMER_ID);
                // 在这里把 State 取出来析构：GDI+ 对象必须在 GdiplusShutdown 之前销毁
                STATE.with(|s| {
                    let _ = s.borrow_mut().take();
                });
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// 所有格子等宽：取「100%」和「GPU0」中较宽者，数字变化时布局不跳动
unsafe fn measure_cell_w(graphics: *mut windows::Win32::Graphics::GdiPlus::GpGraphics, scale: f32) -> f32 {
    let family = render::ui_font_family();
    let Some(big) = FontGuard::new(family, NUM_FONT_PX * scale) else {
        return 70.0 * scale;
    };
    let Some(small) = FontGuard::new(family, PILL_FONT_PX * scale) else {
        return 70.0 * scale;
    };
    let w_big = render::measure_text_width(graphics, big.0, "100%");
    let w_small = render::measure_text_width(graphics, small.0, "GPU0");
    (w_big.max(w_small + 28.0 * scale) + 4.0 * scale).max(24.0)
}

fn main() -> windows::core::Result<()> {
    unsafe {
        // 略低于普通优先级：不抢其他程序的 CPU，又不至于满载时界面卡死不更新
        let _ = SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS);
        // 声明 DPI 感知，否则高分屏上系统会拉伸位图，界面发虚
        let _ = SetProcessDPIAware();

        let mut gdiplus_token = 0usize;
        let startup_input = GdiplusStartupInput {
            GdiplusVersion: 1,
            DebugEventCallback: 0,
            SuppressBackgroundThread: false.into(),
            SuppressExternalCodecs: false.into(),
        };
        if GdiplusStartup(&mut gdiplus_token, &startup_input, std::ptr::null_mut()) != GDIP_OK {
            return Ok(());
        }

        {
            let hinst: HINSTANCE = GetModuleHandleW(None)?.into();
            let class = w!("SysMonWindow");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: hinst,
                lpszClassName: class,
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                ..Default::default()
            };
            RegisterClassW(&wc);

            let scale = dpi_scale();
            let gpu = gpu::GpuMonitor::new();
            let init_gpu: Option<&[f64]> = if gpu.is_some() { Some(&[0.0]) } else { None };
            let cells = build_cells(0.0, init_gpu);

            // 先用一块最小表面量出格子宽度，之后再按实际尺寸重建
            let mut surface = render::LayeredSurface::new();
            surface.ensure(16, 16);
            let cell_w = measure_cell_w(surface.graphics(), scale);

            STATE.with(|s| {
                *s.borrow_mut() = Some(State {
                    cpu: cpu::CpuMonitor::new(),
                    gpu,
                    tick: 0,
                    cells,
                    cell_w,
                    scale,
                    total_w: 0,
                    total_h: 0,
                    surface,
                });
            });

            // 无边框、置顶、不在任务栏显示；分层窗口让圆角逐像素抗锯齿
            let screen_w = GetSystemMetrics(SM_CXSCREEN);
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                class,
                w!("sysmon"),
                WS_POPUP,
                screen_w - 320,
                20,
                200,
                CARD_H as i32,
                None,
                None,
                hinst,
                None,
            )?;

            STATE.with(|s| {
                if let Some(st) = s.borrow_mut().as_mut() {
                    draw(hwnd, st);
                }
            });

            SetTimer(hwnd, TIMER_ID, INTERVAL_MS, None);
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);

            // 消息循环：没有消息时线程休眠，不占 CPU
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        GdiplusShutdown(gdiplus_token);
    }
    Ok(())
}
