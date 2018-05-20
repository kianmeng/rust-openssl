use std::io::{Read, Write};
use std::ops::{Deref, DerefMut};

use dh::Dh;
use error::ErrorStack;
use ssl::{
    HandshakeError, Ssl, SslContext, SslContextBuilder, SslMethod, SslMode, SslOptions, SslRef,
    SslStream, SslVerifyMode,
};
use version;

fn ctx(method: SslMethod) -> Result<SslContextBuilder, ErrorStack> {
    let mut ctx = SslContextBuilder::new(method)?;

    let mut opts = SslOptions::ALL
        | SslOptions::NO_COMPRESSION
        | SslOptions::NO_SSLV2
        | SslOptions::NO_SSLV3
        | SslOptions::SINGLE_DH_USE
        | SslOptions::SINGLE_ECDH_USE
        | SslOptions::CIPHER_SERVER_PREFERENCE;
    opts &= !SslOptions::DONT_INSERT_EMPTY_FRAGMENTS;

    ctx.set_options(opts);

    let mut mode =
        SslMode::AUTO_RETRY | SslMode::ACCEPT_MOVING_WRITE_BUFFER | SslMode::ENABLE_PARTIAL_WRITE;

    // This is quite a useful optimization for saving memory, but historically
    // caused CVEs in OpenSSL pre-1.0.1h, according to
    // https://bugs.python.org/issue25672
    if version::number() >= 0x1_00_01_08_0 {
        mode |= SslMode::RELEASE_BUFFERS;
    }

    ctx.set_mode(mode);

    Ok(ctx)
}

/// A type which wraps client-side streams in a TLS session.
///
/// OpenSSL's default configuration is highly insecure. This connector manages the OpenSSL
/// structures, configuring cipher suites, session options, hostname verification, and more.
///
/// OpenSSL's built in hostname verification is used when linking against OpenSSL 1.0.2 or 1.1.0,
/// and a custom implementation is used when linking against OpenSSL 1.0.1.
#[derive(Clone)]
pub struct SslConnector(SslContext);

impl SslConnector {
    /// Creates a new builder for TLS connections.
    ///
    /// The default configuration is subject to change, and is currently derived from Python.
    pub fn builder(method: SslMethod) -> Result<SslConnectorBuilder, ErrorStack> {
        let mut ctx = ctx(method)?;
        ctx.set_default_verify_paths()?;
        ctx.set_cipher_list(
            "DEFAULT:!aNULL:!eNULL:!MD5:!3DES:!DES:!RC4:!IDEA:!SEED:!aDSS:!SRP:!PSK",
        )?;
        setup_verify(&mut ctx);

        Ok(SslConnectorBuilder(ctx))
    }

    /// Initiates a client-side TLS session on a stream.
    ///
    /// The domain is used for SNI and hostname verification.
    pub fn connect<S>(&self, domain: &str, stream: S) -> Result<SslStream<S>, HandshakeError<S>>
    where
        S: Read + Write,
    {
        self.configure()?.connect(domain, stream)
    }

    /// Returns a structure allowing for configuration of a single TLS session before connection.
    pub fn configure(&self) -> Result<ConnectConfiguration, ErrorStack> {
        Ssl::new(&self.0).map(|ssl| ConnectConfiguration {
            ssl,
            sni: true,
            verify_hostname: true,
        })
    }
}

/// A builder for `SslConnector`s.
pub struct SslConnectorBuilder(SslContextBuilder);

impl SslConnectorBuilder {
    /// Consumes the builder, returning an `SslConnector`.
    pub fn build(self) -> SslConnector {
        SslConnector(self.0.build())
    }
}

impl Deref for SslConnectorBuilder {
    type Target = SslContextBuilder;

    fn deref(&self) -> &SslContextBuilder {
        &self.0
    }
}

impl DerefMut for SslConnectorBuilder {
    fn deref_mut(&mut self) -> &mut SslContextBuilder {
        &mut self.0
    }
}

/// A type which allows for configuration of a client-side TLS session before connection.
pub struct ConnectConfiguration {
    ssl: Ssl,
    sni: bool,
    verify_hostname: bool,
}

impl ConnectConfiguration {
    /// A builder-style version of `set_use_server_name_indication`.
    pub fn use_server_name_indication(mut self, use_sni: bool) -> ConnectConfiguration {
        self.set_use_server_name_indication(use_sni);
        self
    }

    /// Configures the use of Server Name Indication (SNI) when connecting.
    ///
    /// Defaults to `true`.
    pub fn set_use_server_name_indication(&mut self, use_sni: bool) {
        self.sni = use_sni;
    }

    /// A builder-style version of `set_verify_hostname`.
    pub fn verify_hostname(mut self, verify_hostname: bool) -> ConnectConfiguration {
        self.set_verify_hostname(verify_hostname);
        self
    }

    /// Configures the use of hostname verification when connecting.
    ///
    /// Defaults to `true`.
    ///
    /// # Warning
    ///
    /// You should think very carefully before you use this method. If hostname verification is not
    /// used, *any* valid certificate for *any* site will be trusted for use from any other. This
    /// introduces a significant vulnerability to man-in-the-middle attacks.
    pub fn set_verify_hostname(&mut self, verify_hostname: bool) {
        self.verify_hostname = verify_hostname;
    }

    /// Initiates a client-side TLS session on a stream.
    ///
    /// The domain is used for SNI and hostname verification if enabled.
    pub fn connect<S>(mut self, domain: &str, stream: S) -> Result<SslStream<S>, HandshakeError<S>>
    where
        S: Read + Write,
    {
        if self.sni {
            self.ssl.set_hostname(domain)?;
        }

        if self.verify_hostname {
            setup_verify_hostname(&mut self.ssl, domain)?;
        }

        self.ssl.connect(stream)
    }
}

impl Deref for ConnectConfiguration {
    type Target = SslRef;

    fn deref(&self) -> &SslRef {
        &self.ssl
    }
}

impl DerefMut for ConnectConfiguration {
    fn deref_mut(&mut self) -> &mut SslRef {
        &mut self.ssl
    }
}

/// A type which wraps server-side streams in a TLS session.
///
/// OpenSSL's default configuration is highly insecure. This connector manages the OpenSSL
/// structures, configuring cipher suites, session options, and more.
#[derive(Clone)]
pub struct SslAcceptor(SslContext);

impl SslAcceptor {
    /// Creates a new builder configured to connect to non-legacy clients. This should generally be
    /// considered a reasonable default choice.
    ///
    /// This corresponds to the intermediate configuration of Mozilla's server side TLS
    /// recommendations. See its [documentation][docs] for more details on specifics.
    ///
    /// [docs]: https://wiki.mozilla.org/Security/Server_Side_TLS
    pub fn mozilla_intermediate(method: SslMethod) -> Result<SslAcceptorBuilder, ErrorStack> {
        let mut ctx = ctx(method)?;
        #[cfg(ossl111)]
        ctx.set_options(SslOptions::NO_TLSV1_3);
        let dh = Dh::params_from_pem(
            b"
-----BEGIN DH PARAMETERS-----
MIIBCAKCAQEA//////////+t+FRYortKmq/cViAnPTzx2LnFg84tNpWp4TZBFGQz
+8yTnc4kmz75fS/jY2MMddj2gbICrsRhetPfHtXV/WVhJDP1H18GbtCFY2VVPe0a
87VXE15/V8k1mE8McODmi3fipona8+/och3xWKE2rec1MKzKT0g6eXq8CrGCsyT7
YdEIqUuyyOP7uWrat2DX9GgdT0Kj3jlN9K5W7edjcrsZCwenyO4KbXCeAvzhzffi
7MA0BM0oNC9hkXL+nOmFg/+OTxIy7vKBg8P+OxtMb61zO7X8vC7CIAXFjvGDfRaD
ssbzSibBsu/6iGtCOGEoXJf//////////wIBAg==
-----END DH PARAMETERS-----
",
        )?;
        ctx.set_tmp_dh(&dh)?;
        setup_curves(&mut ctx)?;
        ctx.set_cipher_list(
            "ECDHE-ECDSA-CHACHA20-POLY1305:ECDHE-RSA-CHACHA20-POLY1305:\
             ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-GCM-SHA256:\
             ECDHE-ECDSA-AES256-GCM-SHA384:ECDHE-RSA-AES256-GCM-SHA384:\
             DHE-RSA-AES128-GCM-SHA256:DHE-RSA-AES256-GCM-SHA384:ECDHE-ECDSA-AES128-SHA256:\
             ECDHE-RSA-AES128-SHA256:ECDHE-ECDSA-AES128-SHA:ECDHE-RSA-AES256-SHA384:\
             ECDHE-RSA-AES128-SHA:ECDHE-ECDSA-AES256-SHA384:ECDHE-ECDSA-AES256-SHA:\
             ECDHE-RSA-AES256-SHA:DHE-RSA-AES128-SHA256:DHE-RSA-AES128-SHA:DHE-RSA-AES256-SHA256:\
             DHE-RSA-AES256-SHA:ECDHE-ECDSA-DES-CBC3-SHA:ECDHE-RSA-DES-CBC3-SHA:\
             EDH-RSA-DES-CBC3-SHA:AES128-GCM-SHA256:AES256-GCM-SHA384:AES128-SHA256:AES256-SHA256:\
             AES128-SHA:AES256-SHA:DES-CBC3-SHA:!DSS",
        )?;
        Ok(SslAcceptorBuilder(ctx))
    }

    /// Creates a new builder configured to connect to modern clients.
    ///
    /// This corresponds to the modern configuration of Mozilla's server side TLS recommendations.
    /// See its [documentation][docs] for more details on specifics.
    ///
    /// [docs]: https://wiki.mozilla.org/Security/Server_Side_TLS
    pub fn mozilla_modern(method: SslMethod) -> Result<SslAcceptorBuilder, ErrorStack> {
        let mut ctx = ctx(method)?;
        ctx.set_options(SslOptions::NO_TLSV1 | SslOptions::NO_TLSV1_1);
        #[cfg(ossl111)]
        ctx.set_options(SslOptions::NO_TLSV1_3);
        setup_curves(&mut ctx)?;
        ctx.set_cipher_list(
            "ECDHE-ECDSA-AES256-GCM-SHA384:ECDHE-RSA-AES256-GCM-SHA384:\
             ECDHE-ECDSA-CHACHA20-POLY1305:ECDHE-RSA-CHACHA20-POLY1305:\
             ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-GCM-SHA256:ECDHE-ECDSA-AES256-SHA384:\
             ECDHE-RSA-AES256-SHA384:ECDHE-ECDSA-AES128-SHA256:ECDHE-RSA-AES128-SHA256",
        )?;
        Ok(SslAcceptorBuilder(ctx))
    }

    /// Initiates a server-side TLS session on a stream.
    pub fn accept<S>(&self, stream: S) -> Result<SslStream<S>, HandshakeError<S>>
    where
        S: Read + Write,
    {
        let ssl = Ssl::new(&self.0)?;
        ssl.accept(stream)
    }
}

/// A builder for `SslAcceptor`s.
pub struct SslAcceptorBuilder(SslContextBuilder);

impl SslAcceptorBuilder {
    /// Consumes the builder, returning a `SslAcceptor`.
    pub fn build(self) -> SslAcceptor {
        SslAcceptor(self.0.build())
    }
}

impl Deref for SslAcceptorBuilder {
    type Target = SslContextBuilder;

    fn deref(&self) -> &SslContextBuilder {
        &self.0
    }
}

impl DerefMut for SslAcceptorBuilder {
    fn deref_mut(&mut self) -> &mut SslContextBuilder {
        &mut self.0
    }
}

cfg_if! {
    if #[cfg(ossl110)] {
        fn setup_curves(_: &mut SslContextBuilder) -> Result<(), ErrorStack> {
            Ok(())
        }
    } else if #[cfg(any(ossl102, libressl))] {
        fn setup_curves(ctx: &mut SslContextBuilder) -> Result<(), ErrorStack> {
            ctx.set_ecdh_auto(true)
        }
    } else {
        fn setup_curves(ctx: &mut SslContextBuilder) -> Result<(), ErrorStack> {
            use ec::EcKey;
            use nid::Nid;

            let curve = EcKey::from_curve_name(Nid::X9_62_PRIME256V1)?;
            ctx.set_tmp_ecdh(&curve)
        }
    }
}

cfg_if! {
    if #[cfg(any(ossl102, libressl261))] {
        fn setup_verify(ctx: &mut SslContextBuilder) {
            ctx.set_verify(SslVerifyMode::PEER);
        }

        fn setup_verify_hostname(ssl: &mut SslRef, domain: &str) -> Result<(), ErrorStack> {
            use x509::verify::X509CheckFlags;

            let param = ssl.param_mut();
            param.set_hostflags(X509CheckFlags::NO_PARTIAL_WILDCARDS);
            match domain.parse() {
                Ok(ip) => param.set_ip(ip),
                Err(_) => param.set_host(domain),
            }
        }
    } else {
        fn setup_verify(ctx: &mut SslContextBuilder) {
            ctx.set_verify_callback(SslVerifyMode::PEER, verify::verify_callback);
        }

        fn setup_verify_hostname(ssl: &mut Ssl, domain: &str) -> Result<(), ErrorStack> {
            let domain = domain.to_string();
            ssl.set_ex_data(*verify::HOSTNAME_IDX, domain);
            Ok(())
        }

        mod verify {
            use std::net::IpAddr;
            use std::str;

            use ex_data::Index;
            use nid::Nid;
            use ssl::Ssl;
            use stack::Stack;
            use x509::{
                GeneralName, X509NameRef, X509Ref, X509StoreContext, X509StoreContextRef,
                X509VerifyResult,
            };

            lazy_static! {
                pub static ref HOSTNAME_IDX: Index<Ssl, String> = Ssl::new_ex_index().unwrap();
            }

            pub fn verify_callback(preverify_ok: bool, x509_ctx: &mut X509StoreContextRef) -> bool {
                if !preverify_ok || x509_ctx.error_depth() != 0 {
                    return preverify_ok;
                }

                let ok = match (
                    x509_ctx.current_cert(),
                    X509StoreContext::ssl_idx()
                        .ok()
                        .and_then(|idx| x509_ctx.ex_data(idx))
                        .and_then(|ssl| ssl.ex_data(*HOSTNAME_IDX)),
                ) {
                    (Some(x509), Some(domain)) => verify_hostname(domain, &x509),
                    _ => true,
                };

                if !ok {
                    x509_ctx.set_error(X509VerifyResult::APPLICATION_VERIFICATION);
                }

                ok
            }

            fn verify_hostname(domain: &str, cert: &X509Ref) -> bool {
                match cert.subject_alt_names() {
                    Some(names) => verify_subject_alt_names(domain, names),
                    None => verify_subject_name(domain, &cert.subject_name()),
                }
            }

            fn verify_subject_alt_names(domain: &str, names: Stack<GeneralName>) -> bool {
                let ip = domain.parse();

                for name in &names {
                    match ip {
                        Ok(ip) => {
                            if let Some(actual) = name.ipaddress() {
                                if matches_ip(&ip, actual) {
                                    return true;
                                }
                            }
                        }
                        Err(_) => {
                            if let Some(pattern) = name.dnsname() {
                                if matches_dns(pattern, domain) {
                                    return true;
                                }
                            }
                        }
                    }
                }

                false
            }

            fn verify_subject_name(domain: &str, subject_name: &X509NameRef) -> bool {
                match subject_name.entries_by_nid(Nid::COMMONNAME).next() {
                    Some(pattern) => {
                        let pattern = match str::from_utf8(pattern.data().as_slice()) {
                            Ok(pattern) => pattern,
                            Err(_) => return false,
                        };

                        // Unlike SANs, IP addresses in the subject name don't have a
                        // different encoding.
                        match domain.parse::<IpAddr>() {
                            Ok(ip) => pattern
                                .parse::<IpAddr>()
                                .ok()
                                .map_or(false, |pattern| pattern == ip),
                            Err(_) => matches_dns(pattern, domain),
                        }
                    }
                    None => false,
                }
            }

            fn matches_dns(mut pattern: &str, mut hostname: &str) -> bool {
                // first strip trailing . off of pattern and hostname to normalize
                if pattern.ends_with('.') {
                    pattern = &pattern[..pattern.len() - 1];
                }
                if hostname.ends_with('.') {
                    hostname = &hostname[..hostname.len() - 1];
                }

                matches_wildcard(pattern, hostname).unwrap_or_else(|| pattern == hostname)
            }

            fn matches_wildcard(pattern: &str, hostname: &str) -> Option<bool> {
                // internationalized domains can't involved in wildcards
                if pattern.starts_with("xn--") {
                    return None;
                }

                let wildcard_location = match pattern.find('*') {
                    Some(l) => l,
                    None => return None,
                };

                let mut dot_idxs = pattern.match_indices('.').map(|(l, _)| l);
                let wildcard_end = match dot_idxs.next() {
                    Some(l) => l,
                    None => return None,
                };

                // Never match wildcards if the pattern has less than 2 '.'s (no *.com)
                //
                // This is a bit dubious, as it doesn't disallow other TLDs like *.co.uk.
                // Chrome has a black- and white-list for this, but Firefox (via NSS) does
                // the same thing we do here.
                //
                // The Public Suffix (https://www.publicsuffix.org/) list could
                // potentially be used here, but it's both huge and updated frequently
                // enough that management would be a PITA.
                if dot_idxs.next().is_none() {
                    return None;
                }

                // Wildcards can only be in the first component
                if wildcard_location > wildcard_end {
                    return None;
                }

                let hostname_label_end = match hostname.find('.') {
                    Some(l) => l,
                    None => return None,
                };

                // check that the non-wildcard parts are identical
                if pattern[wildcard_end..] != hostname[hostname_label_end..] {
                    return Some(false);
                }

                let wildcard_prefix = &pattern[..wildcard_location];
                let wildcard_suffix = &pattern[wildcard_location + 1..wildcard_end];

                let hostname_label = &hostname[..hostname_label_end];

                // check the prefix of the first label
                if !hostname_label.starts_with(wildcard_prefix) {
                    return Some(false);
                }

                // and the suffix
                if !hostname_label[wildcard_prefix.len()..].ends_with(wildcard_suffix) {
                    return Some(false);
                }

                Some(true)
            }

            fn matches_ip(expected: &IpAddr, actual: &[u8]) -> bool {
                match *expected {
                    IpAddr::V4(ref addr) => actual == addr.octets(),
                    IpAddr::V6(ref addr) => actual == addr.octets(),
                }
            }
        }
    }
}
