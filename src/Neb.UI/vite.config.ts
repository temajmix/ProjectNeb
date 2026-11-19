import { defineConfig, type Plugin } from "vite";
import pkg from "./package.json";
import contract from "./ui-contract.json";

/**
 * The height of the window drag band is a contract between the dashboard and the WPF host:
 * `ui-contract.json` is the single source, read here for the CSS build and embedded into
 * `Neb.App` for `MainWindow.CaptionBandHeight`. CSS writes `__CAPTION_BAND__` instead of a number.
 */
function captionBand(): Plugin {
  const height = `${contract.captionBandHeight}px`;

  return {
    name: "neb-caption-band",
    enforce: "pre",
    transform(code, id) {
      if (!id.endsWith(".css") || !code.includes("__CAPTION_BAND__")) {
        return null;
      }

      return { code: code.replaceAll("__CAPTION_BAND__", height), map: null };
    },
  };
}

export default defineConfig({
  plugins: [captionBand()],
  define: {
    // The badge shows the package version until the host answers `app.version`.
    __APP_VERSION__: JSON.stringify(pkg.version),
  },
  server: { port: 5173, strictPort: true },
  preview: { port: 4173, strictPort: true },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    target: "chrome110",
    assetsInlineLimit: 2048,
  },
});
