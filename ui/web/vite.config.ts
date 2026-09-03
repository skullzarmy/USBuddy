import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// Fixed output filenames: the Rust runtime embeds dist/index.html,
// dist/assets/app.js and dist/assets/styles.css via include_str! and serves
// them at those exact routes. No content hashes — the binary IS the cache key.
export default defineConfig({
    plugins: [react(), tailwindcss()],
    build: {
        rollupOptions: {
            output: {
                entryFileNames: "assets/app.js",
                chunkFileNames: "assets/[name].js",
                assetFileNames: (info) =>
                    info.names?.some((n) => n.endsWith(".css")) ? "assets/styles.css" : "assets/[name][extname]",
            },
        },
    },
    server: {
        // Dev convenience: proxy API + icon to a locally running runtime.
        proxy: {
            "/api": "http://127.0.0.1:8765",
            "/assets/icon.png": "http://127.0.0.1:8765",
        },
    },
    test: {
        environment: "node",
        include: ["tests/**/*.test.ts"],
    },
});
