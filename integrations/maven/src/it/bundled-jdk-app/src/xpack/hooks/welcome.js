// Runs once, after the first installation: leaves a note in the
// installation's data directory, which the application can read.
export function main(ctx) {
    ctx.file.write(ctx.path(ctx.dataDir, "welcomed"), ctx.toVersion);
    ctx.log.info(`welcomed ${ctx.toVersion}`);
}
