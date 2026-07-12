import { fileURLToPath } from "node:url";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

const benchmarkServer = process.env.SANDBOX_BENCHMARK_BIND ?? "127.0.0.1:7891";

export default defineConfig({
  plugins: [
    react(),
    {
      name: "benchmark-fixture-development-nonce",
      apply: "serve",
      transformIndexHtml(html) {
        return html.replace(
          '<meta name="eos-benchmark-nonce" content="" />',
          '<meta name="eos-benchmark-nonce" content="fixture-development-nonce" />',
        );
      },
    },
  ],
  resolve: {
    alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) },
  },
  server: {
    proxy: {
      "/api": { target: `http://${benchmarkServer}`, changeOrigin: false },
    },
  },
});
