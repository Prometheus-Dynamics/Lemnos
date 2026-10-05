//! A minimal consumer of the `lemnos` facade: build, refresh, report. Its
//! stripped size tracks what Lemnos costs a Linux image; an empty `main`
//! built the same way is the baseline it is compared with.
use lemnos::prelude::*;

fn main() {
    #[allow(unused_mut)]
    let mut builder = Lemnos::builder();
    #[cfg(feature = "builtin")]
    {
        builder = builder.with_builtin_drivers().expect("builtin drivers");
    }
    #[cfg(feature = "linux")]
    let backend = LinuxBackend::default();
    #[cfg(feature = "linux")]
    {
        builder = builder.with_linux_backend_ref(&backend);
    }
    #[allow(unused_mut)]
    let mut lemnos = builder.build();
    #[cfg(feature = "linux")]
    let _ = lemnos.refresh_with_linux(&DiscoveryContext::new(), &backend);
    println!("{}", lemnos.inventory_len());
}
