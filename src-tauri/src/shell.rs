//! 用系统默认程序打开文件 / 在资源管理器中定位文件。
//!
//! Windows 上正确的做法是 `ShellExecuteW`——它会走文件关联，等价于用户在
//! 资源管理器里双击。不走 `cmd /C start`：那条路的引号规则和 `&`、`,` 之类的
//! 字符会互相打架；`explorer.exe <path>` 直接传参也有 `,` 被当成参数分隔符的老问题。
//!
//! 只依赖 `shell32`，不引入额外的 crate。

use std::path::Path;

#[cfg(windows)]
mod imp {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    const SW_SHOWNORMAL: i32 = 1;

    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteW(
            hwnd: *mut core::ffi::c_void,
            op: *const u16,
            file: *const u16,
            params: *const u16,
            dir: *const u16,
            show: i32,
        ) -> *mut core::ffi::c_void;
    }

    fn wide(s: &OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    /// 返回值 <= 32 表示失败（ShellExecuteW 的历史约定）。
    fn check(ret: *mut core::ffi::c_void, what: &str) -> Result<(), String> {
        let code = ret as isize;
        if code <= 32 {
            return Err(format!("{what}失败（ShellExecute 返回 {code}）"));
        }
        Ok(())
    }

    pub fn open(path: &Path) -> Result<(), String> {
        let op = wide(OsStr::new("open"));
        let file = wide(path.as_os_str());
        let ret = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                op.as_ptr(),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        check(ret, "打开文件")
    }

    /// 在资源管理器里选中该文件（而不是打开它）。
    pub fn reveal(path: &Path) -> Result<(), String> {
        let op = wide(OsStr::new("open"));
        let explorer = wide(OsStr::new("explorer.exe"));
        // `/select,` 后面必须紧跟路径，整段作为一个参数
        let params = wide(OsStr::new(&format!("/select,\"{}\"", path.display())));
        let ret = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                op.as_ptr(),
                explorer.as_ptr(),
                params.as_ptr(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        check(ret, "打开所在文件夹")
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::Path;
    pub fn open(path: &Path) -> Result<(), String> {
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        std::process::Command::new(opener)
            .arg(path)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    pub fn reveal(path: &Path) -> Result<(), String> {
        // 非 Windows 上没有统一的「选中」语义，打开父目录即可
        let dir = path.parent().unwrap_or(path);
        open(dir)
    }
}

/// 用系统默认程序打开文件。
///
/// **先检查存在性**：文件被移走/删掉时双击不该静默无反应——
/// 那正是本项目反复强调要避免的失败方式。
pub fn open_file(path: &str) -> Result<(), String> {
    let p = Path::new(path);
    if !p.exists() {
        return Err(format!("文件不存在（可能已被移动或删除）：{path}"));
    }
    imp::open(p)
}

/// 在资源管理器中选中该文件。
pub fn reveal_file(path: &str) -> Result<(), String> {
    let p = Path::new(path);
    if !p.exists() {
        return Err(format!("文件不存在（可能已被移动或删除）：{path}"));
    }
    imp::reveal(p)
}

/// 弹原生「选择文件夹」对话框。返回 `None` 表示用户取消。
///
/// ⚠️ **必须从非主线程调用**（命令侧用 `spawn_blocking`）：对话框是模态且阻塞的，
/// 跑在主线程上会把整个窗口连同消息循环一起卡住。
pub fn pick_folder(initial: Option<&str>) -> Result<Option<String>, String> {
    let mut d = rfd::FileDialog::new().set_title("选择文件夹");
    if let Some(dir) = initial.map(str::trim).filter(|s| !s.is_empty()) {
        // 传进来的通常就是当前值；它可能已经不存在了，rfd 会忽略无效的起始目录
        d = d.set_directory(dir);
    }
    Ok(d.pick_folder().map(|p| p.display().to_string()))
}

/// 弹原生「选择文件」对话框（用于归档文件、cookies.txt 这类路径）。
pub fn pick_file(initial: Option<&str>) -> Result<Option<String>, String> {
    let mut d = rfd::FileDialog::new().set_title("选择文件");
    if let Some(dir) = initial
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|s| Path::new(s).parent())
    {
        if dir.is_dir() {
            d = d.set_directory(dir);
        }
    }
    Ok(d.pick_file().map(|p| p.display().to_string()))
}
