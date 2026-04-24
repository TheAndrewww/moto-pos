fn main() {
    let password = "admin123";
    let hash = "$2b$12$N9qo8uLOickgx2ZMRZoMyeIjZAgcfl7p92ldGxad68LJZdL17lhWy";
    match bcrypt::verify(password, hash) {
        Ok(true) => println!("✓ Password matches!"),
        Ok(false) => println!("✗ Password does NOT match"),
        Err(e) => println!("✗ Error: {}", e),
    }
}
