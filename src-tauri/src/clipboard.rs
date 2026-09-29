//! 读系统剪贴板。
//!
//! ## 为什么非要在后端做
//!
//! 「粘贴」是右键菜单里唯一前端做不了的项：
//!
//! - `navigator.clipboard.readText()` 在 WebView2 里会**卡住**——实测它等在一个
//!   `edge://permission-request-dialog/` 权限弹窗上，没人点就永远不返回；
//! - `document.execCommand('paste')` 恒返回 `false`（Chromium 出于安全禁掉了）。
//!
//! 而输入框正是粘贴最常用的地方（添加链接）。所以走 Win32 剪贴板 API：
//! **不引入额外 crate**，只 `user32` + `kernel32`，与 `shell.rs` 同样的取舍。
//!
//! **写**剪贴板不需要走这里：`navigator.clipboard.writeText()` 只要用户手势就能用，
//! 实测真实左键点击后确实写进去了（见 DESIGN §9.0 的验证方式）。

#[cfg(windows)]
mod imp {
    const CF_UNICODETEXT: u32 = 13;
    /// `OpenClipboard` 会被别的进程短暂独占，重试是标准做法。
    const MAX_TRIES: u32 = 10;

    #[link(name = "user32")]
    extern "system" {
        fn OpenClipboard(hwnd: *mut core::ffi::c_void) -> i32;
        fn CloseClipboard() -> i32;
        fn IsClipboardFormatAvailable(format: u32) -> i32;
        fn GetClipboardData(format: u32) -> *mut core::ffi::c_void;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalLock(handle: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
        fn GlobalUnlock(handle: *mut core::ffi::c_void) -> i32;
    }

    /// 读之前必须已经 `OpenClipboard`。
    ///
    /// 剪贴板里没有文本（空的、或只有图片）时返回**空串而不是错误**——
    /// 那对用户来说是正常情况，不是故障。
    unsafe fn read_locked() -> Result<String, String> {
        if IsClipboardFormatAvailable(CF_UNICODETEXT) == 0 {
            return Ok(String::new());
        }
        let handle = GetClipboardData(CF_UNICODETEXT);
        if handle.is_null() {
            return Ok(String::new());
        }
        let ptr = GlobalLock(handle) as *const u16;
        if ptr.is_null() {
            return Ok(String::new());
        }

        // CF_UNICODETEXT 保证以 NUL 结尾
        let mut len = 0usize;
        while *ptr.add(len) != 0 {
            len += 1;
        }
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
        GlobalUnlock(handle);
        Ok(text)
    }

    pub fn read_text() -> Result<String, String> {
        // null hwnd：读操作不关联具体窗口
        let mut opened = false;
        for _ in 0..MAX_TRIES {
            if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if !opened {
            return Err("剪贴板被其他程序占用，稍后再试".into());
        }
        let result = unsafe { read_locked() };
        unsafe {
            CloseClipboard();
        }
        result
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn read_text() -> Result<String, String> {
        // 非 Windows 上没有等价的无依赖实现；右键菜单会退化成没有「粘贴」这一项
        Ok(String::new())
    }
}

/// 读剪贴板里的纯文本。空串表示「没有文本可粘贴」。
pub fn read_text() -> Result<String, String> {
    imp::read_text()
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// 只验证「能调通、不 panic、拿到的是合法 UTF-8 字符串」。
    ///
    /// **不**断言具体内容：剪贴板是全局的，并行跑测试时别的进程随时可能改它，
    /// 断言内容会变成一条偶发失败的用例（这个项目已经被偶发用例咬过一次）。
    #[test]
    fn read_text_returns_something_without_panicking() {
        match read_text() {
            Ok(s) => {
                // 长度是字符数不是字节数，能取到就说明 UTF-16 解码没炸
                let _ = s.chars().count();
            }
            // 被别的程序独占是允许的结果（会走重试后返回这个错）
            Err(e) => assert!(e.contains("占用"), "非预期错误：{e}"),
        }
    }
}
