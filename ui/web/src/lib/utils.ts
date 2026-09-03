import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
    return twMerge(clsx(inputs));
}

export function gib(bytes: number, digits = 1): string {
    return (bytes / 1024 ** 3).toFixed(digits);
}
