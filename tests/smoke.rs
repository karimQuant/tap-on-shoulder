mod support;

#[test]
fn version_prints() {
    let env = support::TestEnv::new(&support::MockNtfy::refused_url());
    let out = env.run(&["--version"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("tap "));
}

#[test]
fn mock_ntfy_records_posts() {
    let mock = support::MockNtfy::start(support::Mode::Ok);
    let body = r#"{"topic":"t","message":"m"}"#;
    let addr = mock.url.trim_start_matches("http://");
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    use std::io::Write;
    write!(
        s,
        "POST / HTTP/1.1\r\nhost: x\r\ncontent-length: {}\r\n\r\n{}",
        body.len(),
        body
    )
    .unwrap();
    assert_eq!(mock.wait_for(1, std::time::Duration::from_secs(2)), 1);
    assert_eq!(mock.requests()[0].json()["topic"], "t");
}
