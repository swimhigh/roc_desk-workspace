#[cfg(windows)]
fn main() {
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK};
    let (name, description) = roc_desk_workspace::tool_info();
    let text = HSTRING::from(format!("{description}\n\n独立工具窗口已启动。业务模块正在迁移接入。"));
    let title = HSTRING::from(name);
    unsafe { MessageBoxW(None, &text, &title, MB_OK | MB_ICONINFORMATION); }
}

#[cfg(not(windows))]
fn main() {
    let (name, description) = roc_desk_workspace::tool_info();
    println!("{name}: {description}");
    std::thread::park();
}
