import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

export default function(pi: ExtensionAPI) {
	pi.on("before_agent_start", async (_event, _ctx) => {
		const result = await pi.exec("./zig-out/bin/tk", ["prime"], { timeout: 10_000 });
		const prompt = result.stdout.trimEnd();
		return {
			message: {
				customType: "tk-prime",
				content: prompt,
				display: false,
			}
		}
	});
}
