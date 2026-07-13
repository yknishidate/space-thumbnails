//! Smoke-tests the render helper's --server pipe protocol end to end:
//! spawns the helper, sends several file paths over one persistent process
//! (exercising engine reuse), and checks each response is a full RGBA frame.
//!
//! usage: cargo run --example client -- <helper.exe> <size> <file.abc> [more...]

use std::{
    io::{Read, Write},
    process::{Command, Stdio},
};

fn main() {
    let mut args = std::env::args().skip(1);
    let helper = args.next().expect("helper exe path");
    let size: u32 = args.next().expect("size").parse().unwrap();
    let files: Vec<String> = args.collect();
    assert!(!files.is_empty(), "need at least one file");

    let mut child = Command::new(&helper)
        .arg("--server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn helper");
    let mut input = child.stdin.take().unwrap();
    let mut output = child.stdout.take().unwrap();

    for file in &files {
        let path = file.as_bytes();
        input.write_all(&(path.len() as u32).to_le_bytes()).unwrap();
        input.write_all(&size.to_le_bytes()).unwrap();
        input.write_all(path).unwrap();
        input.flush().unwrap();

        let mut header = [0u8; 8];
        output.read_exact(&mut header).unwrap();
        let status = i32::from_le_bytes(header[..4].try_into().unwrap());
        let len = u32::from_le_bytes(header[4..].try_into().unwrap()) as usize;
        let mut payload = vec![0u8; len];
        output.read_exact(&mut payload).unwrap();

        let expected = (size * size * 4) as usize;
        if status == 0 && payload.len() == expected {
            println!("OK   {} -> {} bytes", file, len);
        } else if status == 0 {
            println!("BAD  {} -> {} bytes (expected {})", file, len, expected);
        } else {
            println!("ERR  {} -> {}", file, String::from_utf8_lossy(&payload));
        }
    }

    drop(input);
    let _ = child.wait();
}
