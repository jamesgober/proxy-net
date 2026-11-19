//! Pingora forwarding every request to the bench origin.

use async_trait::async_trait;
use pingora::prelude::*;

struct Origin;

#[async_trait]
impl ProxyHttp for Origin {
    type CTX = ();

    fn new_ctx(&self) {}

    async fn upstream_peer(&self, _session: &mut Session, _ctx: &mut ()) -> Result<Box<HttpPeer>> {
        Ok(Box::new(HttpPeer::new(("127.0.0.1", 9000), false, String::new())))
    }
}

fn main() {
    let mut server = Server::new(Some(Opt::parse_args())).expect("server");
    server.bootstrap();
    let mut proxy = http_proxy_service(&server.configuration, Origin);
    proxy.add_tcp("127.0.0.1:8084");
    server.add_service(proxy);
    server.run_forever();
}
