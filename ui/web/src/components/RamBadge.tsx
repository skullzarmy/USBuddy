import { useAppStore, selectedModel } from "../store";
import { archKvBytesPerTokenF16, FALLBACK_KV_BYTES_PER_TOKEN, previewFit } from "../lib/ramfit";
import { gib } from "../lib/utils";
import { Tooltip, TooltipContent, TooltipTrigger } from "./ui/tooltip";

const bandClasses = {
    green: "bg-ok/15 text-ok border-ok/40",
    yellow: "bg-warn/15 text-warn border-warn/40",
    red: "bg-danger/15 text-danger border-danger/40",
} as const;

const bandDots = { green: "🟢", yellow: "🟡", red: "🔴" } as const;

export function RamBadge() {
    const status = useAppStore((s) => s.status);
    const contextTokens = useAppStore((s) => s.contextTokens);
    const model = useAppStore(selectedModel);

    if (!status || !model) return null;
    if (!model.sizeBytes) {
        return <span className="text-xs text-mute">unknown size</span>;
    }

    const kvPerToken = model.arch ? archKvBytesPerTokenF16(model.arch) : FALLBACK_KV_BYTES_PER_TOKEN;
    const fit = previewFit(model.sizeBytes, contextTokens, kvPerToken, status.ram.available_bytes);

    const archNote = model.arch
        ? `${model.arch.architecture} · ${model.arch.block_count}L · ${model.arch.head_count_kv}/${model.arch.head_count} KV/heads · ${kvPerToken.toLocaleString()} B/tok`
        : `unknown arch · assuming ${kvPerToken.toLocaleString()} B/tok (non-GQA worst case)`;

    return (
        <Tooltip>
            <TooltipTrigger asChild>
                <span
                    className={`inline-flex w-fit cursor-default items-center gap-1.5 rounded-full border px-2.5 py-0.5 text-xs font-medium ${bandClasses[fit.band]}`}
                >
                    {bandDots[fit.band]} {fit.label}
                </span>
            </TooltipTrigger>
            {/* Transparent breakdown — the exact math the advisor used. */}
            <TooltipContent side="bottom" className="font-mono text-[11px]">
                <div>
                    model {gib(model.sizeBytes, 2)} GiB + KV {gib(fit.kvBytes, 2)} GiB ({contextTokens.toLocaleString()}{" "}
                    ctx) + overhead 0.50 GiB = {gib(fit.requiredBytes, 2)} GiB needed
                </div>
                <div>
                    available {gib(status.ram.available_bytes, 2)} GiB → {gib(fit.remainingBytes, 2)} GiB headroom
                    (margin {(fit.margin * 100).toFixed(0)}%)
                </div>
                <div className="mt-1 text-mute">{archNote}</div>
            </TooltipContent>
        </Tooltip>
    );
}
