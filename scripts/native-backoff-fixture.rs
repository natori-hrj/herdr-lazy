use std::io::{self, Write};
use std::time::Duration;

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["--version"] => println!("herdr 0.9.5"),
        ["plugin", "--help"] => println!("plugin list --json"),
        ["plugin", "list", "--json"] => println!(r#"{{"result":{{"plugins":[]}}}}"#),
        _ => std::process::exit(1),
    }
    io::stdout().flush().unwrap();
    io::stderr().flush().unwrap();
    close_output();
    let delay = std::env::var("HERDR_BENCH_DELAY_MS")
        .unwrap()
        .parse()
        .unwrap();
    std::thread::sleep(Duration::from_millis(delay));
}

#[cfg(unix)]
fn close_output() {
    extern "C" {
        fn close(fd: i32) -> i32;
    }
    unsafe {
        close(1);
        close(2);
    }
}

#[cfg(windows)]
fn close_output() {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(kind: u32) -> *mut c_void;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }
    unsafe {
        CloseHandle(GetStdHandle(-11_i32 as u32));
        CloseHandle(GetStdHandle(-12_i32 as u32));
    }
}
