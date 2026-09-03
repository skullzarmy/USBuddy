import type { ArchMeta } from "./api";

/// Conservative KV bytes/token when we couldn't parse the GGUF header.
/// Assumes non-GQA (full KV heads). Better for the advisor to err on the
/// side of "tight" than to silently green-light a model that will OOM.
export const FALLBACK_KV_BYTES_PER_TOKEN = 524_288;

export const RUNTIME_OVERHEAD_BYTES = 512 * 1024 * 1024;

export function archKvBytesPerTokenF16(arch: ArchMeta): number {
    const headDim = Math.floor(arch.embedding_length / Math.max(1, arch.head_count));
    return 2 * arch.block_count * arch.head_count_kv * headDim * 2;
}

export type FitBand = "green" | "yellow" | "red";

export interface FitPreview {
    band: FitBand;
    label: string;
    requiredBytes: number;
    kvBytes: number;
    remainingBytes: number;
    margin: number;
}

/// Mirrors the band thresholds used by usbuddy-core's RAM-fit advisor so
/// the sidebar preview matches what /api/launch will decide.
export function previewFit(
    sizeBytes: number,
    contextTokens: number,
    kvBytesPerToken: number,
    availableBytes: number,
): FitPreview {
    const kv = contextTokens * kvBytesPerToken;
    const required = sizeBytes + kv + RUNTIME_OVERHEAD_BYTES;
    const remaining = availableBytes - required;
    const margin = remaining / required;

    let band: FitBand;
    let label: string;
    if (remaining < 0 || remaining < 1_073_741_824) {
        band = "red";
        label = "Won\u2019t fit";
    } else if (margin < 0.2 || remaining < 3_221_225_472) {
        band = "yellow";
        label = "Tight fit";
    } else {
        band = "green";
        label = "Good fit";
    }
    return { band, label, requiredBytes: required, kvBytes: kv, remainingBytes: remaining, margin };
}
