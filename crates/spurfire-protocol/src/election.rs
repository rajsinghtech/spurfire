//! Cross-platform deterministic authority election (`election_v1`).

use std::{fmt, str::FromStr};

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use crate::{
    ConnectivitySample, ConnectivityValidationError, PlayerId, UnixMillis, WireVersion,
    WIRE_VERSION,
};

/// Formula identifier. Any clamp or weight change requires a new value.
pub const AUTHORITY_FORMULA_VERSION: &str = "election_v1";
/// A report is fresh only while its age is strictly less than 60 seconds.
pub const MEASUREMENT_FRESHNESS_MS: u64 = 60_000;
/// A player must have been in the lobby for at least 30 seconds to be eligible.
pub const MIN_AUTHORITY_MEMBERSHIP_MS: u64 = 30_000;

const LOSS_ELIGIBILITY_LIMIT_MILLI: u32 = 5_000;
const MEDIAN_RTT_ELIGIBILITY_LIMIT_MS: u32 = 150;
const CANONICAL_PREFIX: &[u8] = b"spurfire-authority\0election_v1\0";

/// Candidate metadata and the latest complete measurement row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityCandidate {
    /// Candidate identity and deterministic tie-break key.
    pub player_id: PlayerId,
    /// Candidate's advertised wire version.
    pub wire_version: WireVersion,
    /// Time the candidate entered this lobby roster.
    pub joined_at: UnixMillis,
    /// Latest server-timestamped connectivity report.
    pub measurement: ConnectivitySample,
}

/// Normalized inputs, penalties, and final integer score for one candidate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityScoreBreakdown {
    /// Direct route normalization (`D`), 0 through 1,000.
    pub direct_milli: u32,
    /// Median RTT normalization (`M`), 0 through 1,000.
    pub median_rtt_milli: u32,
    /// Worst RTT normalization (`W`), 0 through 1,000.
    pub worst_rtt_milli: u32,
    /// Jitter normalization (`J`), 0 through 1,000.
    pub jitter_milli: u32,
    /// Loss normalization (`L`), 0 through 1,000.
    pub loss_milli: u32,
    /// Upload normalization (`U`), 0 through 1,000.
    pub upload_milli: u32,
    /// Device performance normalization (`P`), 0 through 1,000.
    pub device_performance_milli: u32,
    /// Weighted score before relay penalties.
    pub weighted_score_milli: u32,
    /// Penalty from DERP routes.
    pub derp_relay_penalty_milli: u32,
    /// Penalty from peer-relay routes.
    pub peer_relay_penalty_milli: u32,
    /// Sum of relay penalties.
    pub relay_penalty_milli: u32,
    /// Weighted score minus relay penalty, floored at zero.
    pub score_milli: u32,
}

/// A reason a fresh, complete candidate fails the first-pass eligibility filter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum CandidateIneligibility {
    /// Packet loss is greater than 5 percent (5,000 milli-percent).
    ExcessiveLoss {
        /// Reported loss.
        loss_pct_milli: u32,
    },
    /// Median RTT is greater than 150 milliseconds.
    ExcessiveMedianRtt {
        /// Reported median RTT.
        rtt_ms_median: u32,
    },
    /// Candidate joined less than 30 seconds before election.
    MembershipTooYoung {
        /// Membership duration at election time.
        membership_age_ms: u64,
    },
    /// Candidate's wire major differs from the election peer's major.
    WireMajorMismatch {
        /// Election peer's major version.
        expected_major: u16,
        /// Candidate's major version.
        candidate_major: u16,
    },
    /// Candidate cannot currently reach any other peer.
    ZeroReachablePeers,
}

/// Full score detail for one fresh and complete candidate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScoredAuthorityCandidate {
    /// Candidate identity.
    pub player_id: PlayerId,
    /// Integer score derivation.
    pub breakdown: AuthorityScoreBreakdown,
    /// Empty when the candidate passes the normal eligibility filter.
    pub ineligibility_reasons: Vec<CandidateIneligibility>,
}

/// Compact score exposed by the authority API.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityScore {
    /// Candidate identity.
    pub player_id: PlayerId,
    /// Final integer score.
    pub score_milli: u32,
}

/// Why a measurement row was rejected before scoring.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum CandidateRejectionReason {
    /// Receipt time is later than the supplied deterministic election time.
    MeasurementFromFuture,
    /// Receipt age is at least the 60-second freshness bound.
    StaleMeasurement {
        /// Age at election time.
        age_ms: u64,
    },
    /// Required ranges or peer route rows are incomplete.
    InvalidMeasurement {
        /// Exact validation failure.
        error: ConnectivityValidationError,
    },
}

/// Candidate omitted because its measurement row cannot be used.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedAuthorityCandidate {
    /// Rejected identity.
    pub player_id: PlayerId,
    /// Stable rejection reason.
    pub reason: CandidateRejectionReason,
}

/// Deterministic election output with both API summary and auditable details.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityElection {
    /// Always [`AUTHORITY_FORMULA_VERSION`] for this implementation.
    pub formula_version: String,
    /// Candidates participating after normal filtering, sorted by `PlayerId`.
    /// When `degraded` is true, this contains every fresh, complete candidate.
    pub eligible: Vec<AuthorityScore>,
    /// Highest score, with smallest `PlayerId` bytes winning an exact tie.
    pub winner_player_id: PlayerId,
    /// SHA-256 fingerprint of the canonical player-ID-sorted input matrix.
    pub input_hash: InputHash,
    /// True when no candidate passed normal eligibility and raw scores were used.
    pub degraded: bool,
    /// Complete score derivations, sorted by `PlayerId`.
    pub scored_candidates: Vec<ScoredAuthorityCandidate>,
    /// Stale, future, malformed, or incomplete rows, sorted by `PlayerId`.
    pub rejected_candidates: Vec<RejectedAuthorityCandidate>,
}

/// Election cannot produce a safe deterministic winner.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum AuthorityElectionError {
    /// Authority requires at least two roster members.
    #[error("authority election requires at least two candidates")]
    NotEnoughCandidates,
    /// A canonical matrix may contain only one row per player.
    #[error("duplicate authority candidate {player_id}")]
    DuplicatePlayer {
        /// Duplicated identity.
        player_id: PlayerId,
    },
    /// Every candidate had a stale, future, malformed, or incomplete row.
    #[error("no candidate has a fresh, complete measurement")]
    NoFreshCompleteCandidates,
}

/// Validated lowercase SHA-256 hash used to detect asymmetric matrix views.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InputHash([u8; 32]);

impl InputHash {
    /// Builds a hash value from raw SHA-256 bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the raw digest.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    fn digest(input: &[u8]) -> Self {
        Self(sha256(input))
    }
}

/// Invalid hexadecimal SHA-256 wire representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum InputHashParseError {
    /// A SHA-256 digest has exactly 64 hexadecimal characters.
    #[error("input hash must contain exactly 64 hexadecimal characters")]
    InvalidLength,
    /// Hash contains a non-hexadecimal character.
    #[error("input hash contains a non-hexadecimal character")]
    InvalidHex,
}

impl fmt::Display for InputHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for InputHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("InputHash")
            .field(&self.to_string())
            .finish()
    }
}

impl FromStr for InputHash {
    type Err = InputHashParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 64 {
            return Err(InputHashParseError::InvalidLength);
        }

        fn nibble(byte: u8) -> Result<u8, InputHashParseError> {
            match byte {
                b'0'..=b'9' => Ok(byte - b'0'),
                b'a'..=b'f' => Ok(byte - b'a' + 10),
                b'A'..=b'F' => Ok(byte - b'A' + 10),
                _ => Err(InputHashParseError::InvalidHex),
            }
        }

        let source = value.as_bytes();
        let mut bytes = [0_u8; 32];
        let mut index = 0;
        while index < bytes.len() {
            bytes[index] = (nibble(source[index * 2])? << 4) | nibble(source[index * 2 + 1])?;
            index += 1;
        }
        Ok(Self(bytes))
    }
}

impl Serialize for InputHash {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for InputHash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct HashVisitor;

        impl de::Visitor<'_> for HashVisitor {
            type Value = InputHash;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a 64-character hexadecimal SHA-256 hash")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                value.parse().map_err(E::custom)
            }
        }

        deserializer.deserialize_str(HashVisitor)
    }
}

/// Returns the canonical, player-ID-sorted, fixed-width big-endian matrix bytes.
///
/// Format: domain/formula prefix, `u32` row count, then for each row: 16 UUID
/// bytes, wire major/minor (`u16` each), joined/measured timestamps (`u64` each),
/// followed by ten `u32` measurement integers in protocol field order.
pub fn canonical_measurement_matrix(
    candidates: &[AuthorityCandidate],
) -> Result<Vec<u8>, AuthorityElectionError> {
    let sorted = sorted_candidates(candidates)?;
    let mut bytes = Vec::with_capacity(CANONICAL_PREFIX.len() + 4 + sorted.len() * 80);
    bytes.extend_from_slice(CANONICAL_PREFIX);
    bytes.extend_from_slice(&(sorted.len() as u32).to_be_bytes());
    for candidate in sorted {
        bytes.extend_from_slice(candidate.player_id.as_bytes());
        bytes.extend_from_slice(&candidate.wire_version.major().to_be_bytes());
        bytes.extend_from_slice(&candidate.wire_version.minor().to_be_bytes());
        bytes.extend_from_slice(&candidate.joined_at.as_millis().to_be_bytes());
        bytes.extend_from_slice(&candidate.measurement.measured_at.as_millis().to_be_bytes());
        for value in [
            candidate.measurement.route_summary.direct_count,
            candidate.measurement.route_summary.peer_relay_count,
            candidate.measurement.route_summary.derp_count,
            candidate.measurement.rtt_ms_median,
            candidate.measurement.rtt_ms_worst,
            candidate.measurement.jitter_ms,
            candidate.measurement.loss_pct_milli,
            candidate.measurement.upload_mbps_sustained,
            candidate.measurement.device_perf_score,
            candidate.measurement.observed_peer_count,
        ] {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
    }
    Ok(bytes)
}

/// SHA-256 hash of [`canonical_measurement_matrix`].
pub fn authority_input_hash(
    candidates: &[AuthorityCandidate],
) -> Result<InputHash, AuthorityElectionError> {
    canonical_measurement_matrix(candidates).map(|matrix| InputHash::digest(&matrix))
}

/// Computes one candidate's `election_v1` score without freshness or eligibility filtering.
pub fn score_authority_candidate(
    candidate: &AuthorityCandidate,
    roster_size: usize,
) -> Result<AuthorityScoreBreakdown, ConnectivityValidationError> {
    if roster_size < 2 {
        return Err(ConnectivityValidationError::InvalidRosterSize { roster_size });
    }
    candidate.measurement.validate_for_roster(roster_size)?;
    let peer_count = u32::try_from(roster_size - 1)
        .map_err(|_| ConnectivityValidationError::InvalidRosterSize { roster_size })?;
    Ok(score_measurement(&candidate.measurement, peer_count))
}

/// Elects against [`WIRE_VERSION`] at a caller-supplied deterministic time.
pub fn elect_authority(
    candidates: &[AuthorityCandidate],
    now: UnixMillis,
) -> Result<AuthorityElection, AuthorityElectionError> {
    elect_authority_for_wire(candidates, now, WIRE_VERSION)
}

/// Elects against an explicit local wire version, useful during rolling upgrades.
pub fn elect_authority_for_wire(
    candidates: &[AuthorityCandidate],
    now: UnixMillis,
    expected_wire_version: WireVersion,
) -> Result<AuthorityElection, AuthorityElectionError> {
    if candidates.len() < 2 {
        return Err(AuthorityElectionError::NotEnoughCandidates);
    }
    let sorted = sorted_candidates(candidates)?;
    let input_hash = authority_input_hash(candidates)?;
    let mut scored = Vec::with_capacity(sorted.len());
    let mut rejected = Vec::new();

    for candidate in sorted {
        if let Err(error) = candidate.measurement.validate_for_roster(candidates.len()) {
            rejected.push(RejectedAuthorityCandidate {
                player_id: candidate.player_id,
                reason: CandidateRejectionReason::InvalidMeasurement { error },
            });
            continue;
        }

        let Some(measurement_age_ms) =
            now.checked_duration_since(candidate.measurement.measured_at)
        else {
            rejected.push(RejectedAuthorityCandidate {
                player_id: candidate.player_id,
                reason: CandidateRejectionReason::MeasurementFromFuture,
            });
            continue;
        };
        if measurement_age_ms >= MEASUREMENT_FRESHNESS_MS {
            rejected.push(RejectedAuthorityCandidate {
                player_id: candidate.player_id,
                reason: CandidateRejectionReason::StaleMeasurement {
                    age_ms: measurement_age_ms,
                },
            });
            continue;
        }

        let peer_count = u32::try_from(candidates.len() - 1)
            .expect("candidate count was already represented by measurement u32 fields");
        let breakdown = score_measurement(&candidate.measurement, peer_count);
        let membership_age_ms = now.checked_duration_since(candidate.joined_at).unwrap_or(0);
        let mut reasons = Vec::new();
        if candidate.measurement.loss_pct_milli > LOSS_ELIGIBILITY_LIMIT_MILLI {
            reasons.push(CandidateIneligibility::ExcessiveLoss {
                loss_pct_milli: candidate.measurement.loss_pct_milli,
            });
        }
        if candidate.measurement.rtt_ms_median > MEDIAN_RTT_ELIGIBILITY_LIMIT_MS {
            reasons.push(CandidateIneligibility::ExcessiveMedianRtt {
                rtt_ms_median: candidate.measurement.rtt_ms_median,
            });
        }
        if membership_age_ms < MIN_AUTHORITY_MEMBERSHIP_MS {
            reasons.push(CandidateIneligibility::MembershipTooYoung { membership_age_ms });
        }
        if !candidate
            .wire_version
            .is_compatible_with(expected_wire_version)
        {
            reasons.push(CandidateIneligibility::WireMajorMismatch {
                expected_major: expected_wire_version.major(),
                candidate_major: candidate.wire_version.major(),
            });
        }
        if candidate.measurement.observed_peer_count == 0 {
            reasons.push(CandidateIneligibility::ZeroReachablePeers);
        }
        scored.push(ScoredAuthorityCandidate {
            player_id: candidate.player_id,
            breakdown,
            ineligibility_reasons: reasons,
        });
    }

    if scored.is_empty() {
        return Err(AuthorityElectionError::NoFreshCompleteCandidates);
    }

    let mut eligible: Vec<AuthorityScore> = scored
        .iter()
        .filter(|candidate| candidate.ineligibility_reasons.is_empty())
        .map(|candidate| AuthorityScore {
            player_id: candidate.player_id,
            score_milli: candidate.breakdown.score_milli,
        })
        .collect();
    let degraded = eligible.is_empty();
    if degraded {
        eligible.extend(scored.iter().map(|candidate| AuthorityScore {
            player_id: candidate.player_id,
            score_milli: candidate.breakdown.score_milli,
        }));
    }

    let winner_player_id = eligible
        .iter()
        .min_by(|left, right| {
            right
                .score_milli
                .cmp(&left.score_milli)
                .then_with(|| left.player_id.cmp(&right.player_id))
        })
        .expect("at least one fresh complete candidate was scored")
        .player_id;

    Ok(AuthorityElection {
        formula_version: AUTHORITY_FORMULA_VERSION.to_owned(),
        eligible,
        winner_player_id,
        input_hash,
        degraded,
        scored_candidates: scored,
        rejected_candidates: rejected,
    })
}

fn sorted_candidates(
    candidates: &[AuthorityCandidate],
) -> Result<Vec<&AuthorityCandidate>, AuthorityElectionError> {
    let mut sorted: Vec<_> = candidates.iter().collect();
    sorted.sort_unstable_by_key(|candidate| candidate.player_id);
    for pair in sorted.windows(2) {
        if pair[0].player_id == pair[1].player_id {
            return Err(AuthorityElectionError::DuplicatePlayer {
                player_id: pair[0].player_id,
            });
        }
    }
    Ok(sorted)
}

fn score_measurement(measurement: &ConnectivitySample, peer_count: u32) -> AuthorityScoreBreakdown {
    let direct_milli = normalize_positive(measurement.route_summary.direct_count, peer_count);
    let median_rtt_milli = normalize_inverse(measurement.rtt_ms_median, 200);
    let worst_rtt_milli = normalize_inverse(measurement.rtt_ms_worst, 400);
    let jitter_milli = normalize_inverse(measurement.jitter_ms, 50);
    let loss_milli = normalize_inverse(measurement.loss_pct_milli, 10_000);
    let upload_milli = normalize_positive(measurement.upload_mbps_sustained, 20);
    let device_performance_milli = measurement.device_perf_score.min(1_000);

    let weighted_score_milli = ((270_u64 * u64::from(direct_milli))
        + (230_u64 * u64::from(median_rtt_milli))
        + (140_u64 * u64::from(worst_rtt_milli))
        + (90_u64 * u64::from(jitter_milli))
        + (90_u64 * u64::from(loss_milli))
        + (90_u64 * u64::from(upload_milli))
        + (90_u64 * u64::from(device_performance_milli)))
    .div_euclid(1_000) as u32;
    let derp_relay_penalty_milli = measurement
        .route_summary
        .derp_count
        .min(peer_count)
        .saturating_mul(300)
        .div_euclid(peer_count);
    let peer_relay_penalty_milli = measurement
        .route_summary
        .peer_relay_count
        .min(peer_count)
        .saturating_mul(150)
        .div_euclid(peer_count);
    let relay_penalty_milli = derp_relay_penalty_milli + peer_relay_penalty_milli;
    let score_milli = weighted_score_milli.saturating_sub(relay_penalty_milli);

    AuthorityScoreBreakdown {
        direct_milli,
        median_rtt_milli,
        worst_rtt_milli,
        jitter_milli,
        loss_milli,
        upload_milli,
        device_performance_milli,
        weighted_score_milli,
        derp_relay_penalty_milli,
        peer_relay_penalty_milli,
        relay_penalty_milli,
        score_milli,
    }
}

fn normalize_positive(value: u32, maximum: u32) -> u32 {
    value.min(maximum).saturating_mul(1_000).div_euclid(maximum)
}

fn normalize_inverse(value: u32, maximum: u32) -> u32 {
    maximum
        .saturating_sub(value.min(maximum))
        .saturating_mul(1_000)
        .div_euclid(maximum)
}

// Small dependency-free SHA-256 for the protocol fingerprint. This is not used
// as a password primitive; test vectors still pin it to FIPS 180-4 behavior.
fn sha256(input: &[u8]) -> [u8; 32] {
    const INITIAL: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    const K: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];

    let bit_length = (input.len() as u64).wrapping_mul(8);
    let mut padded = input.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_length.to_be_bytes());

    let mut state = INITIAL;
    for chunk in padded.chunks_exact(64) {
        let mut schedule = [0_u32; 64];
        for (index, word) in chunk.chunks_exact(4).enumerate() {
            schedule[index] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for index in 16..64 {
            let s0 = schedule[index - 15].rotate_right(7)
                ^ schedule[index - 15].rotate_right(18)
                ^ (schedule[index - 15] >> 3);
            let s1 = schedule[index - 2].rotate_right(17)
                ^ schedule[index - 2].rotate_right(19)
                ^ (schedule[index - 2] >> 10);
            schedule[index] = schedule[index - 16]
                .wrapping_add(s0)
                .wrapping_add(schedule[index - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let sum1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ ((!e) & g);
            let temporary1 = h
                .wrapping_add(sum1)
                .wrapping_add(choose)
                .wrapping_add(K[index])
                .wrapping_add(schedule[index]);
            let sum0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temporary2 = sum0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temporary1);
            d = c;
            c = b;
            b = a;
            a = temporary1.wrapping_add(temporary2);
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }

    let mut output = [0_u8; 32];
    for (chunk, word) in output.chunks_exact_mut(4).zip(state) {
        chunk.copy_from_slice(&word.to_be_bytes());
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RouteSummary;

    const NOW: UnixMillis = UnixMillis::new(100_000);

    fn player(number: u64) -> PlayerId {
        PlayerId::parse(&format!("00000000-0000-4000-8000-{number:012x}")).unwrap()
    }

    fn candidate(number: u64, peer_count: u32) -> AuthorityCandidate {
        AuthorityCandidate {
            player_id: player(number),
            wire_version: WIRE_VERSION,
            joined_at: UnixMillis::new(0),
            measurement: ConnectivitySample {
                route_summary: RouteSummary {
                    direct_count: peer_count,
                    peer_relay_count: 0,
                    derp_count: 0,
                },
                rtt_ms_median: 30,
                rtt_ms_worst: 60,
                jitter_ms: 3,
                loss_pct_milli: 100,
                upload_mbps_sustained: 20,
                device_perf_score: 800,
                observed_peer_count: peer_count,
                measured_at: UnixMillis::new(99_000),
            },
        }
    }

    #[test]
    fn sha256_matches_standard_vectors() {
        assert_eq!(
            InputHash::digest(b"").to_string(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            InputHash::digest(b"abc").to_string(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn election_and_hash_ignore_input_order() {
        let mut matrix = vec![
            candidate(4, 3),
            candidate(1, 3),
            candidate(3, 3),
            candidate(2, 3),
        ];
        matrix[1].measurement.rtt_ms_median = 10;
        let first = elect_authority(&matrix, NOW).unwrap();
        matrix.reverse();
        let second = elect_authority(&matrix, NOW).unwrap();
        let third = elect_authority(&matrix, NOW).unwrap();
        assert_eq!(first.winner_player_id, player(1));
        assert_eq!(first.winner_player_id, second.winner_player_id);
        assert_eq!(second.winner_player_id, third.winner_player_id);
        assert_eq!(first.input_hash, second.input_hash);
        assert_eq!(second.input_hash, third.input_hash);
        assert_eq!(
            first.input_hash.to_string(),
            "1e3c5eb49314416556a902c3ef17cdc891dfe77a810bc9dd6fe492fc82f361eb"
        );
    }

    #[test]
    fn exact_score_tie_uses_smallest_player_id_bytes() {
        let election = elect_authority(&[candidate(2, 1), candidate(1, 1)], NOW).unwrap();
        assert_eq!(election.winner_player_id, player(1));
        assert_eq!(
            election.eligible[0].score_milli,
            election.eligible[1].score_milli
        );
    }

    #[test]
    fn relay_penalties_are_explicit_and_change_winner() {
        let direct = candidate(1, 1);
        let mut derp = candidate(2, 1);
        derp.measurement.route_summary = RouteSummary {
            direct_count: 0,
            peer_relay_count: 0,
            derp_count: 1,
        };
        let election = elect_authority(&[derp, direct], NOW).unwrap();
        assert_eq!(election.winner_player_id, player(1));
        let derp_score = election
            .scored_candidates
            .iter()
            .find(|candidate| candidate.player_id == player(2))
            .unwrap();
        assert_eq!(derp_score.breakdown.derp_relay_penalty_milli, 300);
        assert_eq!(derp_score.breakdown.relay_penalty_milli, 300);
    }

    #[test]
    fn excessive_loss_excludes_an_otherwise_fast_candidate() {
        let mut lossy = candidate(1, 2);
        lossy.measurement.rtt_ms_median = 0;
        lossy.measurement.rtt_ms_worst = 0;
        lossy.measurement.device_perf_score = 1_000;
        lossy.measurement.loss_pct_milli = 5_001;
        let election = elect_authority(&[lossy, candidate(2, 2), candidate(3, 2)], NOW).unwrap();
        assert_ne!(election.winner_player_id, player(1));
        assert!(!election
            .eligible
            .iter()
            .any(|score| score.player_id == player(1)));
        assert!(!election.degraded);
    }

    #[test]
    fn empty_eligibility_filter_falls_back_deterministically() {
        let mut first = candidate(2, 1);
        let mut second = candidate(1, 1);
        first.measurement.rtt_ms_median = 151;
        second.measurement.rtt_ms_median = 151;
        let election = elect_authority(&[first, second], NOW).unwrap();
        assert!(election.degraded);
        assert_eq!(election.winner_player_id, player(1));
        assert_eq!(election.eligible.len(), 2);
    }

    #[test]
    fn zero_reachable_peers_use_deterministic_degraded_fallback() {
        let mut first = candidate(2, 1);
        let mut second = candidate(1, 1);
        for candidate in [&mut first, &mut second] {
            candidate.measurement.route_summary.direct_count = 0;
            candidate.measurement.observed_peer_count = 0;
        }
        let election = elect_authority(&[first, second], NOW).unwrap();
        assert!(election.degraded);
        assert_eq!(election.winner_player_id, player(1));
        assert!(election.scored_candidates.iter().all(|candidate| {
            candidate
                .ineligibility_reasons
                .contains(&CandidateIneligibility::ZeroReachablePeers)
        }));
    }

    #[test]
    fn stale_and_incomplete_rows_are_rejected_not_used_by_fallback() {
        let mut stale = candidate(1, 1);
        stale.measurement.measured_at = UnixMillis::new(40_000);
        let election = elect_authority(&[stale.clone(), candidate(2, 1)], NOW).unwrap();
        assert_eq!(election.winner_player_id, player(2));
        assert!(matches!(
            election.rejected_candidates[0].reason,
            CandidateRejectionReason::StaleMeasurement { age_ms: 60_000 }
        ));

        let mut incomplete = candidate(2, 1);
        incomplete.measurement.observed_peer_count = 0;
        assert_eq!(
            elect_authority(&[stale, incomplete], NOW),
            Err(AuthorityElectionError::NoFreshCompleteCandidates)
        );
    }

    #[test]
    fn canonical_integer_fields_are_big_endian() {
        let mut row = candidate(1, 1);
        row.measurement.rtt_ms_median = 0x0102_0304;
        let matrix = canonical_measurement_matrix(&[row, candidate(2, 1)]).unwrap();
        let row_start = CANONICAL_PREFIX.len() + 4;
        let median_offset = row_start + 16 + 2 + 2 + 8 + 8 + (3 * 4);
        assert_eq!(
            &matrix[median_offset..median_offset + 4],
            &[0x01, 0x02, 0x03, 0x04]
        );
    }

    #[test]
    fn input_hash_wire_form_round_trips() {
        let hash = authority_input_hash(&[candidate(1, 1), candidate(2, 1)]).unwrap();
        assert_eq!(hash.to_string().parse::<InputHash>().unwrap(), hash);
        assert_eq!(
            serde_json::from_str::<InputHash>(&serde_json::to_string(&hash).unwrap()).unwrap(),
            hash
        );
    }
}
