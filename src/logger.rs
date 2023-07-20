const ESC: &str = "\x1B";

pub fn log_fetching(package_name: &String) {
  print!("{ESC}[1A{ESC}[2K\rfetching: {}\n", package_name);
}

pub fn log_processed(package_name: &String) {
  print!("{ESC}[1A{ESC}[2K\rprocessed: {}\n", package_name);
}