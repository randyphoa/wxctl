const { McpServer } = require("@modelcontextprotocol/sdk/server/mcp.js");
const { StdioServerTransport } = require("@modelcontextprotocol/sdk/server/stdio.js");
const { z } = require("zod");

const server = new McpServer({ name: "hello-server", version: "1.0.0" });

server.tool("hello", "Returns a greeting for the given name", { name: z.string().describe("Name to greet") }, async ({ name }) => ({
  content: [{ type: "text", text: `Hello, ${name}!` }],
}));

async function main() {
  const transport = new StdioServerTransport();
  await server.connect(transport);
}

main();
