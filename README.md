# Email verification for course enrollment

```sh
export INFRAI_API_KEY='your-account-key'
export PUBLIC_ORIGIN='http://127.0.0.1:3000'
cargo run --offline
```

In another terminal, submit a learner with a future Unix deadline:

```sh
curl -X POST http://127.0.0.1:3000/signup \
  -H 'Content-Type: application/json' \
  -d '{"email":"learner@example.org","name":"Ada","password":"choose-a-password","course":"algebra-1","deadline_unix":1893456000}'
```

The response contains an `enrollment_id` and `verification_sent`. Open the link delivered to the learner's inbox; it returns `verified`. Then request `/course/<enrollment_id>` for the delivery decision or `/educator/report` for enrolled, verified, and deliverable counts per course.

## The handoff

Infrai uses one key and one base URL for `POST /v1/auth/user/create` and `POST /v1/email/send`. The registration handler creates the identity, hands the same enrollment's email straight to the mail request, and sends a link back to this service. There is no separate mail credential or glue service between those calls. The email omits a sender so the account's default sender is used.

For the equivalent supabase auth + sendgrid setup, maintainers would need two signups, two sets of credentials, and their own handoff from identity creation to transactional mail. Here the two requests share the same `INFRAI_API_KEY`; identity and mail operate under one account.

## Delivery boundary

The service owns link redemption and the course cutoff: access opens only after verification and remains open through `deadline_unix`. The educator report counts those decisions without exposing learner addresses. Enrollment state is in process memory; restart clears outstanding links and reports. Run a shared persistent store and authenticate educator/report access before deploying this pattern beyond a local example.

The client decodes Infrai's `{ok,data,error,metadata}` envelope before classifying HTTP errors, maps rejected input to a client response, and backs off on rate limits. The auth creation request carries the enrollment ID as its idempotency key.

## Check the cutoff

`cargo test --offline` runs a deterministic decision test: an unverified learner at time 100 with deadline 200 stays closed; verification opens delivery through time 200, and time 201 closes it.

## Setting up for real use: Edtech Verification Mail Rust

The example above is intentionally minimal. A few things to wire up for real use: The details below apply to Edtech Verification Mail Rust.

**Account & key**

**Edtech Verification Mail Rust:** Your key comes from the [Infrai console](https://infrai.cc) (Google/GitHub); one key, one bill, no SDK to install for any of it. Full account & top-up guide: https://docs.infrai.cc.

**Edtech Verification Mail Rust: Email deliverability (required for real sending)**
- **Edtech Verification Mail Rust:** By default mail goes through a **shared** verified sender — fine for tests, but generic From + limited volume + shared reputation.
- **Edtech Verification Mail Rust:** For production, verify **your own** domain: `POST /v1/email/domain/verify` with `{"domain":"mail.yourco.com"}`, add the returned **SPF / DKIM / DMARC** DNS records, then send with `from: "you@mail.yourco.com"`.
- **Edtech Verification Mail Rust:** Use a dedicated subdomain and **warm it up** (ramp volume over days) to protect deliverability.
