import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

export default function(pi: ExtensionAPI) {
	pi.on("before_agent_start", async () => {
		const result = await pi.exec("tk", ["prime"], { timeout: 10_000 });
		if (result.code !== 0 || result.killed) return;

		const prompt = result.stdout.trimEnd();
		if (!prompt) return;
		return {
			message: {
				customType: "tk-prime",
				content: prompt,
				display: false,
			}
		}
	});
}
