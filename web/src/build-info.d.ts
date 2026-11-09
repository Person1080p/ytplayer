// Set by vite.config.ts `define` at build time: the git commit this page was built from.

/** Full commit hash, or null when git wasn't available to the build. */
declare const __APP_COMMIT__: string | null;
/** The working tree had uncommitted changes. */
declare const __APP_DIRTY__: boolean;
/** Build time, unix seconds. */
declare const __APP_BUILT_AT__: number | null;
