// Runs once, before the application is removed.
export function main(ctx) {
    ctx.log.info(`goodbye from ${ctx.fromVersion}`);
}
