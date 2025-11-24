use std::io;

pub fn read_line() -> String {
    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();
    input.trim().to_string() // trim trailing chars that read_line adds.
}
