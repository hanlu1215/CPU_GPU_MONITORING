// 以 GUI 子系统运行：不会弹出控制台窗口
#![windows_subsystem = "windows"]

mod cpu;
mod gpu;

use std::cell::RefCell;
use windows::core::w;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    GetCurrentProcess, SetPriorityClass, BELOW_NORMAL_PRIORITY_CLASS,
};
use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
use windows::Win32::UI::WindowsAndMessaging::*;

// ---------- 可调参数 ----------
const TIMER_ID: usize = 1;
const INTERVAL_MS: u32 = 1000; // 刷新间隔
const GPU_REFRESH_EVERY: u32 = 60; // 每 N 次采样重建一次 GPU 查询
const HOT: f64 = 95.0; // 达到该百分比视为满载，变红

// ---------- 布局 ----------
const PAD_X: i32 = 14; // 左右内边距
const GAP: i32 = 12; // 两格之间的间距
const TOP: i32 = 8;
const NUM_H: i32 = 38; // 大数字行高
const PILL_H: i32 = 22; // 标签胶囊高度
const HEIGHT: i32 = TOP + NUM_H + 2 + PILL_H + 10;
const RADIUS: i32 = 16; // 窗口圆角半径

// ---------- 配色（参考图：深色底、蓝色数字、绿色胶囊）----------
const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}
const C_BG: u32 = rgb(26, 30, 42);
const C_NUM: u32 = rgb(77, 163, 255);
const C_PILL: u32 = rgb(22, 72, 50);
const C_PILL_TXT: u32 = rgb(70, 230, 140);
const C_NUM_HOT: u32 = rgb(255, 90, 90);
const C_PILL_HOT: u32 = rgb(96, 32, 36);
const C_PILL_TXT_HOT: u32 = rgb(255, 130, 130);

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
    font_big: HFONT,
    font_small: HFONT,
    cell_w: i32,
    cells: Vec<Cell>,
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

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// 用指定字体测量文字宽度
unsafe fn measure(font: HFONT, s: &str) -> i32 {
    let hdc = GetDC(None);
    let old = SelectObject(hdc, font);
    let mut sz = SIZE::default();
    let _ = GetTextExtentPoint32W(hdc, &wide(s), &mut sz);
    SelectObject(hdc, old);
    ReleaseDC(None, hdc);
    sz.cx
}

/// 按格子数量调整窗口大小，并设置圆角（位置不变）
unsafe fn fit_window(hwnd: HWND, st: &State) {
    let n = st.cells.len() as i32;
    let w = PAD_X * 2 + n * st.cell_w + (n - 1) * GAP;
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    if rc.right != w || rc.bottom != HEIGHT {
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            w,
            HEIGHT,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
        let rgn = CreateRoundRectRgn(0, 0, w + 1, HEIGHT + 1, RADIUS * 2, RADIUS * 2);
        SetWindowRgn(hwnd, rgn, true); // 区域所有权交给系统，无需手动释放
    }
}

/// 双缓冲绘制，避免闪烁
unsafe fn paint(hwnd: HWND, st: &State) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let (w, h) = (rc.right, rc.bottom);

    let mem = CreateCompatibleDC(hdc);
    let bmp = CreateCompatibleBitmap(hdc, w, h);
    let old_bmp = SelectObject(mem, bmp);

    let bg = CreateSolidBrush(COLORREF(C_BG));
    FillRect(mem, &rc, bg);
    let _ = DeleteObject(bg);

    SetBkMode(mem, TRANSPARENT);
    let old_pen = SelectObject(mem, GetStockObject(NULL_PEN)); // 胶囊不要描边
    let old_font = SelectObject(mem, st.font_small);

    for (i, c) in st.cells.iter().enumerate() {
        let x0 = PAD_X + i as i32 * (st.cell_w + GAP);

        // 大数字
        SelectObject(mem, st.font_big);
        SetTextColor(mem, COLORREF(if c.hot { C_NUM_HOT } else { C_NUM }));
        let mut r = RECT { left: x0, top: TOP, right: x0 + st.cell_w, bottom: TOP + NUM_H };
        let mut ws = wide(&c.value);
        DrawTextW(mem, &mut ws, &mut r, DT_CENTER | DT_VCENTER | DT_SINGLELINE);

        // 胶囊底色
        let pill_top = TOP + NUM_H + 2;
        let brush = CreateSolidBrush(COLORREF(if c.hot { C_PILL_HOT } else { C_PILL }));
        let old_brush = SelectObject(mem, brush);
        let _ = RoundRect(mem, x0, pill_top, x0 + st.cell_w, pill_top + PILL_H, PILL_H, PILL_H);
        SelectObject(mem, old_brush);
        let _ = DeleteObject(brush);

        // 胶囊文字
        SelectObject(mem, st.font_small);
        SetTextColor(mem, COLORREF(if c.hot { C_PILL_TXT_HOT } else { C_PILL_TXT }));
        let mut r2 = RECT { left: x0, top: pill_top, right: x0 + st.cell_w, bottom: pill_top + PILL_H };
        let mut wl = wide(&c.label);
        DrawTextW(mem, &mut wl, &mut r2, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
    }

    let _ = BitBlt(hdc, 0, 0, w, h, mem, 0, 0, SRCCOPY);

    SelectObject(mem, old_font);
    SelectObject(mem, old_pen);
    SelectObject(mem, old_bmp);
    let _ = DeleteObject(bmp);
    let _ = DeleteDC(mem);
    let _ = EndPaint(hwnd, &ps);
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
                            fit_window(hwnd, st);
                            let _ = InvalidateRect(hwnd, None, false);
                        }
                    }
                });
                LRESULT(0)
            }
            WM_PAINT => {
                STATE.with(|s| {
                    if let Some(st) = s.borrow().as_ref() {
                        paint(hwnd, st);
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
                STATE.with(|s| {
                    if let Some(st) = s.borrow_mut().take() {
                        let _ = DeleteObject(st.font_big);
                        let _ = DeleteObject(st.font_small);
                    }
                });
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

unsafe fn make_font(height: i32, weight: i32) -> HFONT {
    CreateFontW(
        height,
        0,
        0,
        0,
        weight,
        0,
        0,
        0,
        DEFAULT_CHARSET.0 as u32,
        OUT_DEFAULT_PRECIS.0 as u32,
        CLIP_DEFAULT_PRECIS.0 as u32,
        CLEARTYPE_QUALITY.0 as u32,
        0,
        w!("Segoe UI"),
    )
}

fn main() -> windows::core::Result<()> {
    unsafe {
        // 略低于普通优先级：不抢其他程序的 CPU，又不至于满载时界面卡死不更新
        let _ = SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS);

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

        let font_big = make_font(-30, 700);
        let font_small = make_font(-14, 600);
        // 所有格子等宽：取「100%」和「GPU0」胶囊中较宽者，数字变化时布局不跳动
        let cell_w = measure(font_big, "100%").max(measure(font_small, "GPU0") + 28) + 4;

        let gpu = gpu::GpuMonitor::new();
        let init_gpu: Option<&[f64]> = if gpu.is_some() { Some(&[0.0]) } else { None };
        let cells = build_cells(0.0, init_gpu);
        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                cpu: cpu::CpuMonitor::new(),
                gpu,
                tick: 0,
                font_big,
                font_small,
                cell_w,
                cells,
            });
        });

        // 无边框、置顶、不在任务栏显示
        let screen_w = GetSystemMetrics(SM_CXSCREEN);
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class,
            w!("sysmon"),
            WS_POPUP,
            screen_w - 320,
            20,
            200,
            HEIGHT,
            None,
            None,
            hinst,
            None,
        )?;

        STATE.with(|s| {
            if let Some(st) = s.borrow().as_ref() {
                fit_window(hwnd, st);
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
    Ok(())
}
