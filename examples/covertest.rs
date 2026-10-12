use std::io::Read;

fn main() {
    let url = "https://cdn.cloudflare.steamstatic.com/steam/apps/397540/header.jpg";
    println!("GET {}", url);
    let connector = native_tls::TlsConnector::new().expect("init native-tls");
    let agent = ureq::AgentBuilder::new()
        .tls_connector(std::sync::Arc::new(connector))
        .build();
    match agent.get(url).timeout(std::time::Duration::from_secs(15)).call() {
        Ok(resp) => {
            println!("status: {}", resp.status());
            let mut buf = Vec::new();
            match resp.into_reader().take(3_000_000).read_to_end(&mut buf) {
                Ok(n) => println!("read {} bytes", n),
                Err(e) => println!("read error: {}", e),
            }
        }
        Err(e) => println!("error: {:?}", e),
    }
}
