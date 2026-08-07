# OAuth rate-limit benchmark and rollout plan

## Purpose

This document is the implementation plan for validating and tuning the fixed OAuth rate-limit defaults before broad release. It is intentionally detailed enough for an engineer or an LLM coding agent to generate the benchmark harness, run it safely, interpret the results, and carry the work through post-merge validation.

The benchmark must answer two different questions:

1. **Protection ceiling:** How many worst-case JWT validations can one Apollo MCP Server process sustain without unacceptable CPU use or latency?
2. **Legitimate floor:** How much valid aggregate authentication traffic can a real deployment send through one peer address, especially when a reverse proxy or NAT is in front of the server?

The selected peer limit needs a comfortable gap between those values:

```text
legitimate peak through one peer
        < peer rate limit
        < process overload threshold
```

If no comfortable gap exists, changing constants is not a sufficient solution. The design will need a better client identity, trusted-proxy support, deployment-specific configuration, or another architectural change.

## Current implementation under test

At the time this plan was written, the rate limiter is implemented in:

- `crates/apollo-mcp-server/src/auth/rate_limit.rs`
- `crates/apollo-mcp-server/src/auth.rs`
- `crates/apollo-mcp-server/src/auth/valid_token.rs`

The fixed, process-local defaults are:

| Layer | Sustained rate | Burst | Key |
| --- | ---: | ---: | --- |
| Credential | 10 requests/second | 50 requests | Process-randomized bearer-token fingerprint |
| Peer safety fuse | 500 requests/second | 1,000 requests | Direct connection peer IP |

The peer fuse deliberately ignores forwarded client-IP headers. All traffic through one reverse proxy, load balancer, or shared NAT therefore shares a single peer bucket. Distinct bearer credentials still have independent credential buckets.

Both bucket maps are capped at 50,000 entries. A new key fails open when its map is full and no idle bucket can be evicted; the other limiter continues to apply. Rejections and fail-open capacity events are observable through:

- `apollo.mcp.auth.rate_limit.count`
- `apollo.mcp.auth.rate_limit.overflow.count`
- The `apollo.mcp.rate_limit_kind` attribute (`credential` or `peer`)
- Warning and error logs emitted immediately, then at most once every 60 seconds per category with a `suppressed` count

Attacker-influenced token-validation warnings use the same 60-second baseline per warning category. This interval is provisional and must be evaluated as part of the pre-merge benchmark.

Before implementing a benchmark, re-read these files and update this section if the implementation has changed. Do not assume that the values or metric names in this document are still current.

## Execution rules for an implementing LLM

An LLM implementing this plan must:

1. Read the repository's `AGENTS.md` or equivalent instructions, `CONTRIBUTING.md`, and the Rust best-practices skill before editing code.
2. Confirm the current branch, working tree status, rate-limit constants, auth middleware order, and `TokenValidator` implementation.
3. Keep benchmark-only visibility changes behind `#[cfg(test)]` when possible. Do not make authentication internals public merely to benchmark them.
4. Reuse the real `TokenValidator::validate` path and the existing in-memory `KeyResolver` test seam. Do not benchmark a substitute implementation of JWT verification.
5. Build and run performance measurements in release mode. Debug-build results are invalid for tuning these constants.
6. Keep key generation, JWT generation, runtime construction, fixture parsing, network setup, and warm-up outside timed intervals.
7. Use clearly marked test-only keys. Never use production credentials, customer tokens, or external authorization servers.
8. Never print or export complete bearer tokens. Result artifacts may contain counts, timings, algorithms, key sizes, and status codes, but not credentials.
9. Run attack-shaped traffic only against localhost, an isolated benchmark environment, or an explicitly authorized staging target.
10. Avoid adding a new runtime dependency. Do not add Criterion or another benchmark dependency unless maintainers explicitly decide that the benchmark should become permanent infrastructure.
11. Do not add hardware-dependent latency or throughput assertions to normal CI. A benchmark may be an ignored release-mode test that prints results for human interpretation.
12. Do not change limiter behavior while generating the benchmark. If the measurements justify new constants or design changes, make those changes separately and rerun the full validation matrix.

## Deliverables

The pre-merge work should produce:

1. A release-mode microbenchmark of the hot-cache JWT validation path.
2. A release-mode end-to-end single-peer load test comparing the base branch with the rate-limit branch.
3. A legitimate shared-proxy workload test using many valid credentials.
4. A result record containing environment details, raw measurements, calculations, conclusions, and a recommendation for the peer rate and burst.
5. A short PR summary linking or copying the result record.

The benchmark harness may be temporary. If it is not suitable for stable CI or routine developer use, remove it before merging and preserve the commands and results in the PR or a separate result document. Keep this plan unless the team decides it is no longer useful.

## Phase 1: pre-merge validation

### Step 1: establish a reproducible environment

Use the smallest production-like deployment size, preferably a Linux container limited to one vCPU. A developer laptop run is useful for catching an obviously unsafe value, but it is not authoritative for production tuning.

Record at least:

| Field | Value |
| --- | --- |
| Git base SHA | |
| Git branch SHA | |
| Working tree clean or dirty | |
| Rust compiler version | |
| Build command and flags | |
| OS and kernel | |
| CPU model | |
| Container CPU limit | |
| Memory limit | |
| JWT algorithms and key sizes | |
| Logging format and level | |
| Telemetry exporter configuration | |
| Load-generator name and version | |

Run all measured processes with stable CPU and memory limits. Avoid unrelated workloads on the benchmark host. Use the same host, configuration, compiler, and load generator for the base and branch comparison.

### Step 2: generate the JWT validation microbenchmark

#### Benchmark target

Benchmark the real `TokenValidator::validate` implementation in `crates/apollo-mcp-server/src/auth/valid_token.rs`. The measurement should include:

- JWT header decoding
- Warm in-memory key resolution
- JWK cloning performed by the resolver
- `Validation` construction
- Signature verification
- Claims deserialization and validation
- Scope extraction when validation succeeds

It must exclude OIDC discovery, JWKS HTTP fetching, fixture construction, and token signing. Those activities have separate controls and would make the cryptographic capacity measurement noisy.

#### Recommended implementation shape

The simplest initial harness is an ignored test inside the existing `#[cfg(test)]` module in `valid_token.rs`. That location can reuse `TestTokenValidator`, `StubKeyResolver`, and private production types without widening visibility.

Use an explicitly manual name and a single-thread Tokio runtime:

```rust
#[tokio::test(flavor = "current_thread")]
#[ignore = "manual release-mode JWT validation benchmark"]
async fn benchmark_hot_cache_invalid_signature_validation() {
    // Build keys, tokens, and validator before timing.
    // Warm the path before collecting samples.
    // Measure several samples with std::time::Instant.
    // Use std::hint::black_box on inputs and results.
    // Print operations/second and time/operation; do not assert a threshold.
}
```

The implementing agent should use `std::hint::black_box`, run at least five samples, and report the median as well as every individual sample. Start with roughly 2,000 warm-up validations and 50,000 measured validations per sample, then increase the iteration count if a sample is too short to be stable.

Validate one representative token before timing so the benchmark fails clearly if fixture construction is wrong. During the measured loop, pass results through `black_box` rather than formatting or logging them.

Run the benchmark alone:

```bash
cargo test -p apollo-mcp-server --release \
  benchmark_hot_cache_invalid_signature_validation \
  -- --ignored --nocapture --test-threads=1
```

#### Required JWT cases

Measure these cases separately; do not mix them into one aggregate:

1. **Valid signature, warm key:** Measures the normal validation path.
2. **Invalid signature, known `kid`, warm key:** Primary CPU-attack case. The JWT must be syntactically valid and have a correctly sized signature produced by a different test key so verification reaches the cryptographic failure.
3. **Malformed JWT:** Lower-cost reference case that fails during parsing.
4. **Unknown `kid`, warm issuer state:** Measures a cache miss after the separate JWKS-refresh guard is active. Do not include an actual network round trip in the crypto microbenchmark.

At minimum, use RS256 with a 2,048-bit test key because it is a common enterprise configuration. Also measure the slowest algorithm and key size that the team expects to support in production. If that is not known, report RS256 separately and do not claim that its result covers every supported algorithm.

The existing auth tests primarily use HS512 helpers. They are useful for validating the harness but are not sufficient evidence for choosing an enterprise-facing peer fuse.

#### Logging variants

Run the invalid-signature case twice:

1. With no tracing subscriber, to isolate validation cost.
2. With production-like tracing and exporter configuration, to measure the real attack path.

Invalid tokens can produce validation warnings. At hundreds of invalid tokens per second, formatting, writing, or exporting those warnings may dominate signature verification, so the logging-enabled measurement is part of the capacity decision rather than an optional follow-up.

The branch starts with a baseline that emits the first validation warning in each category immediately, then emits at most once every 60 seconds with the number suppressed since the preceding warning. Measure this behavior with production-like logging enabled. If it still contributes material cost, or suppresses operationally useful diagnostics for too long, adjust the interval based on the benchmark and repeat the logging-enabled case.

#### Microbenchmark result table

| Case | Algorithm/key | Logging | Median time/op | Median ops/sec | Slowest sample | Notes |
| --- | --- | --- | ---: | ---: | ---: | --- |
| Valid, warm key | | Off | | | | |
| Invalid signature, warm key | | Off | | | | |
| Invalid signature, warm key | | Production-like | | | | |
| Malformed token | | Off | | | | |
| Unknown `kid`, warm state | | Off | | | | |

### Step 3: generate the end-to-end attack-shaped load test

The microbenchmark isolates validation cost, but the peer constants must be tested through the real HTTP and middleware path.

#### Server setup

1. Build the base and branch servers in release mode.
2. Configure `streamable_http` with OAuth enabled.
3. Run a local OIDC discovery endpoint and JWKS endpoint using static test fixtures or a dedicated local fixture server.
4. Use a test issuer, audience, `kid`, and non-production signing keys.
5. Send one warm-up request and confirm the discovery and JWKS caches are warm before measuring.
6. Keep telemetry and logging settings identical between the base and branch runs.
7. Expose and monitor the health endpoint so event-loop responsiveness can be measured independently of the auth response.

The authoritative comparison is between:

- `origin/main` or the actual PR base SHA without the limiter
- The exact rate-limit branch SHA being proposed for merge

Do not compare unrelated builds or different dependency graphs.

#### Attack token corpus

Generate at least 10,000 distinct, correctly formed JWTs with:

- The configured RS256 algorithm and known `kid`
- A valid-looking payload with unique `jti` or equivalent entropy
- A future expiration, expected audience, and required subject
- A correctly sized signature created with the wrong private key

The resulting credentials must be distinct so the 10 requests/second credential bucket does not hide the peer fuse. Reuse the corpus round-robin at a low enough per-token rate that no individual credential hits its bucket.

Do not create malformed base64 signatures for the primary attack case. A malformed signature can fail before expensive cryptographic verification and will overstate how protective a given peer limit is.

#### Load shape

Send all benchmark traffic through one direct peer IP. On localhost, separate TCP source ports still share the same `127.0.0.1` peer bucket.

Run a staircase such as:

```text
100 -> 250 -> 500 -> 750 -> 1,000 -> 2,000 requests/second
```

Hold each level for 30 to 60 seconds. Start each authoritative step with a fresh server process or wait long enough for the token bucket to return to a known full state. Record the initial burst separately from steady state.

Because the peer bucket starts with 1,000 tokens and refills at 500 tokens/second, a saturated test should allow an initial burst and then settle near 500 validations/second. Over a finite interval, the average allowed rate will be higher than 500 because it includes the initial burst:

```text
expected allowed over N seconds <= 1,000 + (500 * N)
```

Do not diagnose the limiter from the first second alone.

#### Measurements

For every load level, record:

- Requested and achieved requests/second
- `2xx`, `401`, `429`, and other response counts
- `Retry-After` presence on a sample of `429` responses
- Process CPU utilization
- Resident memory
- HTTP p50, p95, and p99 latency
- Health-check p50, p95, and p99 latency
- `apollo.mcp.auth.rate_limit.count` by limiter kind
- `apollo.mcp.auth.rate_limit.overflow.count` by limiter kind
- Warning/error log volume and exporter throughput
- Whether the server or load generator saturated first

The branch should behave like the base below the fuse, apart from small limiter overhead. Above the fuse, validations should settle near the configured peer rate, excess requests should become `429`, and the health endpoint should remain responsive.

#### End-to-end result table

| Build | Target req/s | Achieved req/s | `401` | `429` | CPU | RSS | HTTP p99 | Health p99 | Rejection count | Overflow count |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Base | 100 | | | N/A | | | | | N/A | N/A |
| Branch | 100 | | | | | | | | | |
| Base | 250 | | | N/A | | | | | N/A | N/A |
| Branch | 250 | | | | | | | | | |
| Base | 500 | | | N/A | | | | | N/A | N/A |
| Branch | 500 | | | | | | | | | |
| Base | 750 | | | N/A | | | | | N/A | N/A |
| Branch | 750 | | | | | | | | | |
| Base | 1,000 | | | N/A | | | | | N/A | N/A |
| Branch | 1,000 | | | | | | | | | |
| Base | 2,000 | | | N/A | | | | | N/A | N/A |
| Branch | 2,000 | | | | | | | | | |

### Step 4: run a legitimate shared-proxy workload

An attack benchmark determines the highest safe limit; it does not determine the lowest acceptable limit for customers.

Generate valid JWTs for many independent credentials and send them through one peer. Include:

- The largest expected number of concurrently active credentials behind one enterprise proxy
- Expected steady per-credential request rates
- A realistic one- to two-second reconnect or session-initialization burst
- Long-lived steady traffic after the burst

The current 500 requests/second peer fuse is equivalent to examples such as:

- 50 credentials each sustaining 10 requests/second
- 500 credentials each sustaining 1 request/second
- 1,000 credentials each sustaining 0.5 requests/second

The 1,000-request burst absorbs short spikes, but it does not permit aggregate traffic above 500 requests/second indefinitely.

If real customer traffic data is available, derive the test from the p99 per-instance, per-peer one-second arrival rate. If no such data exists, document the concurrency and request-rate assumptions explicitly rather than presenting them as observed behavior.

No legitimate request in the agreed supported workload should receive `429`. Verify that the credential limits do not independently reject a client expected to exceed 10 requests/second sustained or burst beyond 50.

### Step 5: calculate candidate limits

Let `T` be the measured CPU seconds for one worst-case, hot-cache validation. Let `F` be the maximum fraction of one vCPU that one attacking peer should be able to consume continuously.

An initial CPU-budget rate is:

```text
peer_rate = floor(F / T)
```

For example, if the team assigns one peer at most 25% of one vCPU:

| Worst-case validation cost | 500 requests/second consumes | A burst of 1,000 consumes |
| ---: | ---: | ---: |
| 0.25 ms | 12.5% of one core | 250 ms CPU |
| 0.5 ms | 25% of one core | 500 ms CPU |
| 1 ms | 50% of one core | 1 second CPU |
| 2 ms | 100% of one core | 2 seconds CPU |

Choose the sustained rate using both constraints:

```text
supported legitimate peak with headroom
    < selected peer rate
    < measured safe attack ceiling with headroom
```

A useful starting policy, subject to the team's actual SLOs, is:

- At least 2x headroom above the p99 legitimate one-second arrival rate.
- No more than 25% to 40% of the measured single-vCPU saturation throughput for the worst realistic invalid-signature case.
- A burst large enough for the agreed legitimate reconnect spike, but small enough that consuming the entire burst does not monopolize CPU long enough to violate the health or request-latency SLO.

Do not silently adopt these percentages if the team has an existing CPU budget or latency SLO. Use the established production objectives instead.

### Step 6: make the pre-merge decision

The PR is ready to merge only when the result record answers all of these questions:

- What is the worst measured validation case?
- At the proposed peer rate, how much of the smallest supported process's CPU can one peer consume?
- How much CPU time can the full burst consume?
- Does the server remain healthy above the limiter threshold?
- Does the limiter settle at the expected steady-state rate?
- Is limiter overhead acceptable below the threshold?
- Does the supported legitimate shared-proxy workload avoid `429` responses?
- Did any test exhaust a 50,000-entry map?
- Are the values based on production-like hardware and a release build?

Possible outcomes:

1. **Keep 500/1,000:** Measurements show adequate protection and legitimate headroom.
2. **Change the internal constants:** Repeat the end-to-end tests after the change and update documentation and PR text.
3. **Do not merge the always-on design yet:** Legitimate traffic and the safe attack ceiling overlap, or the server cannot remain responsive at the selected threshold.

Record the decision in the PR. Do not commit a machine-specific timing threshold as a normal test assertion.

## Phase 2: post-merge validation

Merging does not finish the performance work. The pre-merge benchmark establishes a safe hypothesis; staging and canary observation check it against realistic deployment behavior.

If merging automatically deploys broadly, complete the production-like pre-merge benchmark before merging. If merge and release are separate, the branch may merge after the pre-merge gate, but broad release should still wait for the staging and canary checks below.

### Step 1: staging soak

Deploy the merged build using production-equivalent CPU and memory limits. Run:

1. The agreed legitimate shared-proxy workload.
2. The rotating-invalid-signature attack workload above the peer threshold.
3. A mixed workload with legitimate traffic and attack traffic sharing the same process.

Run each steady workload for at least 30 minutes, including realistic telemetry export and log processing. Confirm:

- Legitimate traffic receives no unexpected `429` responses.
- Attack traffic is clamped after the initial burst.
- Health and normal request latency remain within the agreed SLO.
- CPU and memory stabilize rather than climbing throughout the soak.
- Rejection metrics match observed `429` responses closely enough to be operationally useful.
- Overflow metrics remain zero under normal traffic.

### Step 2: controlled canary

Roll the change to a small, identifiable set of instances before broad deployment. Observe at least one full business cycle; use a longer period when enterprise traffic has weekly or batch-driven peaks.

Monitor:

- Rate-limit rejection count by `credential` and `peer`
- Rate-limit overflow count by limiter kind
- Overall HTTP request rate and status distribution
- CPU, memory, restarts, and throttling
- HTTP and health-check latency
- Authentication failure and JWKS-refresh behavior
- Log and telemetry exporter volume
- Customer or support reports of intermittent `429` responses

The rate-limit metrics deliberately contain no token or peer-IP label. Do not add sensitive or high-cardinality labels merely to make canary investigation easier. Correlate with instance-level HTTP metrics, timestamped throttled logs, deployment topology, and authorized operational data.

### Step 3: post-merge tuning triggers

Investigate or stop rollout when any of the following occurs:

- Legitimate requests receive peer-limit `429` responses.
- A shared proxy's normal aggregate traffic approaches or exceeds the sustained peer rate.
- Credential-limit `429` responses affect a supported client traffic pattern.
- The server becomes CPU-bound or its health latency violates SLO while attack traffic is clamped.
- `apollo.mcp.auth.rate_limit.overflow.count` is non-zero without a known adversarial or synthetic cause.
- Rate-limiter logs or invalid-token logs create material telemetry cost.

Response guidance:

- If the safe CPU ceiling is lower than expected, lower the peer rate or burst and repeat the benchmark and canary.
- If legitimate traffic needs a higher limit and the measured CPU budget permits it, raise the constants and repeat the validation matrix.
- If legitimate traffic needs a higher limit than the process can safely permit for one peer, do not simply increase the fuse. Revisit peer identity, trusted proxies, horizontal isolation, or a narrowly designed configuration surface.
- If map overflow occurs, determine whether it is ordinary deployment cardinality, an attack, or insufficient idle eviction before changing the 50,000-entry cap.
- If logging dominates CPU or I/O, address log sampling or throttling separately and remeasure.

### Step 4: complete rollout and preserve evidence

Before completing broad rollout:

- Attach the staging and canary result summaries to the tracking issue or PR.
- Record the final constants and the rationale for them.
- Record the tested deployment size and algorithms so future maintainers understand the boundary of the evidence.
- Create a follow-up issue for any deferred trusted-proxy, configurability, logging, or continuous-benchmark work.
- Update this document when the implementation, limits, metrics, or rollout process changes.

## Result record template

Copy this section into a PR comment, issue, or a separate result document.

### Environment

| Field | Value |
| --- | --- |
| Base SHA | |
| Branch SHA | |
| Rust version | |
| OS/kernel | |
| CPU model and limit | |
| Memory limit | |
| Build command | |
| JWT algorithms/key sizes | |
| Logging/exporter setup | |
| Load generator/version | |

### Microbenchmark conclusion

- Worst realistic case:
- Median validation time:
- Median validations/second:
- Cost with production-like logging:
- Estimated CPU at proposed sustained peer rate:
- Estimated CPU cost of a full burst:

### End-to-end conclusion

- Base saturation point:
- Branch steady validation rate under attack:
- Limiter overhead below threshold:
- CPU under sustained attack:
- Health p99 under sustained attack:
- Initial burst behavior:
- Rejection metric accuracy:
- Overflow events:

### Legitimate workload conclusion

- Credentials behind one peer:
- Sustained aggregate request rate:
- Peak one-second request rate:
- Unexpected credential rejections:
- Unexpected peer rejections:
- Customer-data source or documented assumption:

### Decision

- Selected credential rate/burst:
- Selected peer rate/burst:
- Keep, change, or block merge:
- Rationale:
- Remaining uncertainty:
- Required post-merge checks:

## Final checklist

### Before merge

- [ ] Reconfirm the current implementation and constants.
- [ ] Run release-mode hot-cache JWT microbenchmarks.
- [ ] Include an invalid-signature case that reaches cryptographic verification.
- [ ] Measure production-like logging overhead.
- [ ] Compare base and branch end to end on the same host.
- [ ] Run the legitimate shared-proxy workload.
- [ ] Calculate sustained and burst CPU budgets.
- [ ] Confirm health and request-latency behavior above the fuse.
- [ ] Record the environment, measurements, assumptions, and recommendation.
- [ ] Update constants, tests, docs, and PR text if measurements require it.
- [ ] Run `cargo test`, strict Clippy, and formatting checks after any code change.

### After merge

- [ ] Run a production-equivalent staging soak.
- [ ] Test legitimate, adversarial, and mixed workloads.
- [ ] Deploy to a controlled canary.
- [ ] Monitor rejection and overflow metrics by limiter kind.
- [ ] Monitor CPU, memory, latency, restarts, logging, and exporter load.
- [ ] Investigate any legitimate `429` or unexplained overflow.
- [ ] Tune through a follow-up change when measurements justify it.
- [ ] Revisit the client-identity design instead of raising limits beyond the safe CPU ceiling.
- [ ] Preserve results and final rationale for future maintainers.
