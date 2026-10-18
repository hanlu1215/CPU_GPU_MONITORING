//! CPU 总占用：GetSystemTimes 两次采样做差，几乎零开销。

use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::Threading::GetSystemTimes;

fn ft(f: FILETIME) -> u64 {
    ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64
}

/// 返回 (idle, kernel, user)。注意 kernel 已包含 idle。
fn read() -> Option<(u64, u64, u64)> {
    let (mut i, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe { GetSystemTimes(Some(&mut i), Some(&mut k), Some(&mut u)).ok()? };
    Some((ft(i), ft(k), ft(u)))
}

pub struct CpuMonitor {
    prev: (u64, u64, u64),
}

impl CpuMonitor {
    pub fn new() -> Self {
        Self { prev: read().unwrap_or_default() }
    }

    /// 距离上次调用期间的 CPU 占用百分比 (0~100)
    pub fn sample(&mut self) -> f64 {
        let Some(cur) = read() else { return 0.0 };
        let di = cur.0.wrapping_sub(self.prev.0);
        let dk = cur.1.wrapping_sub(self.prev.1);
        let du = cur.2.wrapping_sub(self.prev.2);
        self.prev = cur;
        let total = dk + du;
        if total == 0 {
            return 0.0;
        }
        ((total.saturating_sub(di)) as f64 / total as f64 * 100.0).clamp(0.0, 100.0)
    }
}
