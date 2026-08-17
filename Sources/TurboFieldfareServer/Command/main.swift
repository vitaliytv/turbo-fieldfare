import Darwin
import Foundation
import TurboFieldfareServerCore

let arguments: ServerArguments
do {
    arguments = try ServerArguments.parse(Array(CommandLine.arguments.dropFirst()))
} catch ServerArgumentError.help {
    print(ServerArguments.usage)
    exit(0)
} catch {
    FileHandle.standardError.write(Data("error: \(error)\n\n\(ServerArguments.usage)\n".utf8))
    exit(2)
}

do {
    let signals = ServerTerminationSignals()
    let modelURL = URL(fileURLWithPath: arguments.model).standardizedFileURL
    let backend = try await ServerModelSession.load(
        modelDirectory: modelURL,
        maxContext: arguments.maxContext,
        promptCacheMode: arguments.promptCacheMode)
    let server = TurboFieldfareHTTPServer(
        modelID: arguments.modelID,
        queueLimit: arguments.queueLimit,
        backend: backend)
    _ = try await server.start(port: arguments.port)
    // stderr, like every other runtime line. Under launchd both streams land in
    // the same log file, but stdout is block buffered there: printing the banner
    // left it in the buffer until the process exited, so it surfaced after a
    // whole session of requests and described a server that was already gone.
    FileHandle.standardError.write(Data("""
    TurboFieldfareServer ready at http://127.0.0.1:\(arguments.port) \
    model=\(arguments.modelID) context=\(arguments.maxContext) \
    prompt_cache=\(arguments.promptCacheMode.rawValue)

    """.utf8))

    _ = await signals.wait()
    try await server.shutdown()
    await signals.cancel()
} catch {
    FileHandle.standardError.write(Data("error: \(error)\n".utf8))
    exit(1)
}
