import http from 'node:http'
import readline from 'node:readline'

const isHttp = process.argv[2] === '--http'

function send(message) {
  process.stdout.write(`${JSON.stringify(message)}\n`)
}

function respond(id, result) {
  send({ jsonrpc: '2.0', id, result })
}

function error(id, code, message) {
  send({ jsonrpc: '2.0', id, error: { code, message } })
}

function handleRequest(request) {
  if (request.method === 'notifications/initialized') {
    return null
  }
  if (request.id === undefined || request.id === null) {
    return null
  }

  switch (request.method) {
    case 'initialize':
      return {
        jsonrpc: '2.0',
        id: request.id,
        result: {
          protocolVersion: request.params?.protocolVersion ?? '2025-06-18',
          capabilities: { tools: {} },
          serverInfo: {
            name: 'codeforge-smoke-mcp',
            version: '0.1.0',
          },
        },
      }
    case 'tools/list':
      return {
        jsonrpc: '2.0',
        id: request.id,
        result: {
          tools: [
            {
              name: 'echo',
              description: 'Echo a message for MCP smoke tests.',
              inputSchema: {
                type: 'object',
                properties: {
                  message: { type: 'string' },
                },
              },
            },
          ],
        },
      }
    case 'tools/call': {
      const toolName = request.params?.name
      if (toolName !== 'echo') {
        return {
          jsonrpc: '2.0',
          id: request.id,
          error: { code: -32602, message: `unknown tool: ${toolName}` },
        }
      }
      const message = String(request.params?.arguments?.message ?? '')
      return {
        jsonrpc: '2.0',
        id: request.id,
        result: {
          content: [{ type: 'text', text: `echo:${message}` }],
          structuredContent: { echoed: message },
          isError: false,
        },
      }
    }
    default:
      return {
        jsonrpc: '2.0',
        id: request.id,
        error: { code: -32601, message: `unknown method: ${request.method}` },
      }
  }
}

function startStdio() {
  const rl = readline.createInterface({
    input: process.stdin,
    crlfDelay: Infinity,
  })

  rl.on('line', (line) => {
    if (!line.trim()) {
      return
    }
    let request
    try {
      request = JSON.parse(line)
    } catch {
      return
    }

    const response = handleRequest(request)
    if (response?.error) {
      error(response.id, response.error.code, response.error.message)
      return
    }
    if (response) {
      respond(response.id, response.result)
    }
  })
}

function startHttp() {
  const port = Number(process.argv[3] ?? 0)
  const server = http.createServer((request, response) => {
    if (request.method !== 'POST' || request.url !== '/mcp') {
      response.writeHead(405)
      response.end()
      return
    }

    let body = ''
    request.setEncoding('utf8')
    request.on('data', (chunk) => {
      body += chunk
    })
    request.on('end', () => {
      let parsed
      try {
        parsed = JSON.parse(body)
      } catch {
        response.writeHead(400)
        response.end()
        return
      }

      const message = handleRequest(parsed)
      if (!message) {
        response.writeHead(202)
        response.end()
        return
      }

      response.writeHead(200, { 'content-type': 'application/json' })
      response.end(JSON.stringify(message))
    })
  })

  server.listen(port, '127.0.0.1')
}

if (isHttp) {
  startHttp()
} else {
  startStdio()
}
