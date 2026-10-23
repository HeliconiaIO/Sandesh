//! Where mail goes when no local identity matches.

/// How a recipient matching no local identity is routed.
///
/// `CORE_SPEC.md` §7 specifies these three outcomes by name, which makes the
/// choice domain vocabulary rather than a configuration detail: the routing
/// rules are stated in terms of it, and [`DeliveryService`](crate::DeliveryService)
/// cannot express §7 without it.
///
/// It is mirrored here rather than used from `sandesh-config` because
/// `ARCHITECTURE.md` §3.2 fixes the core's dependencies and configuration is not
/// among them (ADR-071). `sandesh-config` converts its own enum into this one at
/// the composition root, so the two cannot drift silently — a variant added
/// there and not here fails to compile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnknownRecipient {
    /// Deliver to the catch-all account.
    ///
    /// The default: a development mail server that bounces unrecognised
    /// recipients creates friction with no benefit.
    #[default]
    CatchAll,
    /// Create an account and identity on demand.
    AutoCreate,
    /// Refuse the recipient at `RCPT TO`.
    ///
    /// The protocol adapter turns this into its own refusal — `550` for SMTP —
    /// which is why the variant names the decision and not the reply code.
    Strict,
}
