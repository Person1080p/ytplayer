// The commit this page was built from (see vite.config.ts), for Settings → Version.

export const APP_COMMIT: string | null = typeof __APP_COMMIT__ === "string" ? __APP_COMMIT__ : null;
export const APP_DIRTY: boolean = typeof __APP_DIRTY__ === "boolean" ? __APP_DIRTY__ : false;
export const APP_BUILT_AT: number | null = typeof __APP_BUILT_AT__ === "number" ? __APP_BUILT_AT__ : null;
