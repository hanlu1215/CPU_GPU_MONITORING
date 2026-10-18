//! GPU 占用：读取与任务管理器同源的性能计数器
//! `\GPU Engine(*)\Utilization Percentage`（需要 Win10 1709+ 且驱动为 WDDM 2.x+）。
//!
//! 实例名形如：
//!   pid_1234_luid_0x00000000_0x0000A1B2_phys_0_eng_0_engtype_3D
//! 聚合方式（与任务管理器一致）：
//!   1. 同一个物理引擎上，所有进程的占用求和
//!   2. 同一块显卡上，取各引擎的最大值作为该显卡占用

use std::collections::HashMap;
use windows::core::w;
use windows::Win32::System::Performance::*;

pub struct GpuMonitor {
    query: isize,
    counter: isize,
}

impl GpuMonitor {
    /// 不支持（旧系统/旧驱动）时返回 None
    pub fn new() -> Option<Self> {
        unsafe {
            let mut query = 0isize;
            if PdhOpenQueryW(None, 0, &mut query) != 0 {
                return None;
            }
            let mut counter = 0isize;
            let r = PdhAddEnglishCounterW(
                query,
                w!("\\GPU Engine(*)\\Utilization Percentage"),
                0,
                &mut counter,
            );
            if r != 0 {
                PdhCloseQuery(query);
                return None;
            }
            // 先采一次基准值，下次采样才有有效数据
            PdhCollectQueryData(query);
            Some(Self { query, counter })
        }
    }

    /// 返回每块显卡的占用百分比（按 luid 排序，序号即 GPU0/GPU1...）
    pub fn sample(&mut self) -> Vec<f64> {
        let items = unsafe {
            if PdhCollectQueryData(self.query) != 0 {
                return vec![];
            }
            read_array(self.counter)
        };

        // (luid, 物理引擎标识) -> 各进程占用之和
        let mut engines: HashMap<(String, String), f64> = HashMap::new();
        for (name, val) in items {
            let (Some(a), Some(b)) = (name.find("luid_"), name.find("_phys_")) else { continue };
            let luid = name[a + 5..b].to_string();
            let eng = name[b + 1..].to_string(); // phys_0_eng_0_engtype_3D
            *engines.entry((luid, eng)).or_insert(0.0) += val;
        }

        // luid -> 各引擎最大值
        let mut adapters: HashMap<String, f64> = HashMap::new();
        for ((luid, _), v) in engines {
            let e = adapters.entry(luid).or_insert(0.0);
            if v > *e {
                *e = v;
            }
        }

        let mut list: Vec<_> = adapters.into_iter().collect();
        list.sort_by(|a, b| a.0.cmp(&b.0));
        list.into_iter().map(|(_, v)| v.clamp(0.0, 100.0)).collect()
    }
}

impl Drop for GpuMonitor {
    fn drop(&mut self) {
        unsafe {
            PdhCloseQuery(self.query);
        }
    }
}

/// 读取通配符计数器的全部实例值 (实例名, 值)
unsafe fn read_array(counter: isize) -> Vec<(String, f64)> {
    let mut size = 0u32;
    let mut count = 0u32;
    PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut size, &mut count, None);
    if size == 0 {
        return vec![];
    }
    // 用 u64 做缓冲，保证 8 字节对齐
    let mut buf = vec![0u64; (size as usize) / 8 + 1];
    let ptr = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
    let mut size2 = (buf.len() * 8) as u32;
    if PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut size2, &mut count, Some(ptr)) != 0 {
        return vec![];
    }
    std::slice::from_raw_parts(ptr, count as usize)
        .iter()
        .filter(|it| it.FmtValue.CStatus == 0) // 0 = 有效
        .map(|it| {
            (
                it.szName.to_string().unwrap_or_default(),
                it.FmtValue.Anonymous.doubleValue,
            )
        })
        .collect()
}
