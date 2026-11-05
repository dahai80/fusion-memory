import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// #33: 构建产物落 dist/ (提交进仓库, fm-server 编译期内嵌 serve at /app)。
export default defineConfig({
    plugins: [react(), tailwindcss()],
    base: "/app/",
    build: {
        outDir: "dist",
        assetsInlineLimit: 4096,
    },
});
