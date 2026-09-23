//! 播放期间阻止屏保 / 系统休眠（后端兜底）。
//!
//! 首选方案是前端 Wake Lock API（WebView2 / 较新 WKWebView 原生
//! 支持）；本模块只在 WebView 不支持该 API 或申请失败时兜底：
//! - Windows：SetThreadExecutionState（kernel32 手工声明 FFI，
//!   零新增依赖）——断言挂在常驻线程上，收到释放信号或进程
//!   退出时由操作系统自动复位；
//! - macOS：`caffeinate -dims -w <本进程 pid>`（本进程退出后
//!   caffeinate 自动结束，无残留）；
//! - Linux：systemd-inhibit 持锁期间循环确认本进程存活，本进程
//!   （含异常）退出后 inhibitor 自动解除；
//! - 其它平台：空实现（等同不支持，静默降级）。
//!
//! 所有实现均幂等：重复开启 / 关闭无副作用；入口只被前端
//! set_keep_awake 命令调用，命令层见 commands.rs。

/// 开启（active=true）/ 关闭（active=false）「保持唤醒」。
pub fn set_keep_awake(active: bool) {
    imp::set(active);
}

#[cfg(windows)]
mod imp {
    use std::sync::mpsc;
    use std::sync::Mutex;
    use std::thread;

    // SetThreadExecutionState 标志位（winbase.h）
    const ES_CONTINUOUS: u32 = 0x8000_0000;
    const ES_SYSTEM_REQUIRED: u32 = 0x0000_0001;
    const ES_DISPLAY_REQUIRED: u32 = 0x0000_0002;

    // kernel32 手工声明：仅为一条系统调用引入整个 windows-sys 不值得
    #[link(name = "kernel32")]
    extern "system" {
        fn SetThreadExecutionState(esflags: u32) -> u32;
    }

    // 断言持有线程的发送端：drop 即释放（通道关闭 → 线程复位并退出）
    static HOLDER: Mutex<Option<mpsc::Sender<()>>> = Mutex::new(None);

    pub fn set(active: bool) {
        let mut guard = HOLDER.lock().unwrap();
        if active {
            if guard.is_some() {
                return; // 已开启：幂等
            }
            let (tx, rx) = mpsc::channel::<()>();
            thread::spawn(move || {
                unsafe {
                    // 显示器 + 系统双重保持：既不熄屏也不休眠
                    SetThreadExecutionState(
                        ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED,
                    );
                }
                // 阻塞直到释放（发送端 drop）；进程退出时线程随之消亡，
                // 操作系统自动清除断言，无需显式清理
                let _ = rx.recv();
                unsafe {
                    SetThreadExecutionState(ES_CONTINUOUS); // 恢复系统默认
                }
            });
            *guard = Some(tx);
        } else if let Some(tx) = guard.take() {
            drop(tx); // 通道关闭 → 线程复位断言并退出
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::process::{Child, Command, Stdio};
    use std::sync::Mutex;

    static CHILD: Mutex<Option<Child>> = Mutex::new(None);

    pub fn set(active: bool) {
        let mut guard = CHILD.lock().unwrap();
        if active {
            if guard.is_some() {
                return; // 已开启：幂等
            }
            let pid = std::process::id().to_string();
            // caffeinate：-d 阻止显示器休眠 -i 阻止系统空闲休眠
            // -s 阻止系统睡眠（接电源时）-m 阻止磁盘休眠
            // -w <pid> 跟随本进程存活，本进程退出后自动结束
            if let Ok(child) = Command::new("caffeinate")
                .args(["-dims", "-w", &pid])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                *guard = Some(child);
            }
        } else if let Some(mut child) = guard.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::process::{Child, Command, Stdio};
    use std::sync::Mutex;

    static CHILD: Mutex<Option<Child>> = Mutex::new(None);

    pub fn set(active: bool) {
        let mut guard = CHILD.lock().unwrap();
        if active {
            if guard.is_some() {
                return; // 已开启：幂等
            }
            let pid = std::process::id().to_string();
            // systemd-inhibit 持锁期间运行一个「本进程存活循环」：
            // 本进程（含异常）退出后循环结束 → inhibitor 自动解除，
            // 不会残留系统级锁。非 systemd 发行版上 spawn 失败 →
            // 静默降级（等同不支持）。
            let watch = format!("while kill -0 {pid} 2>/dev/null; do sleep 30; done");
            if let Ok(child) = Command::new("systemd-inhibit")
                .args([
                    "--what=sleep:idle",
                    "--mode=block",
                    "--who=jisu-kanpian",
                    "sh",
                    "-c",
                    &watch,
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                *guard = Some(child);
            }
        } else if let Some(mut child) = guard.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

// 其余平台（含移动端）：空实现
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
mod imp {
    pub fn set(_active: bool) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keep_awake_toggle_is_idempotent() {
        // 开 / 关各两次：不 panic、不阻塞、状态复位即可
        // （Linux 沙箱无 systemd 时 spawn 静默失败，同样视为通过）
        set_keep_awake(true);
        set_keep_awake(true);
        set_keep_awake(false);
        set_keep_awake(false);
    }
}
