import 'dart:io';

/// Where the tern daemon's API lives: its Unix socket on this computer. The
/// daemon trusts whoever can open the socket.
sealed class Endpoint {
  const Endpoint();

  const factory Endpoint.unix(String path) = UnixEndpoint;

  /// The local daemon's socket: `$TERN_HOME/tern.sock`, else `~/.tern/tern.sock`.
  static Endpoint local([Map<String, String>? env]) {
    env ??= Platform.environment;
    final home = env['TERN_HOME'] ?? '${env['HOME'] ?? ''}/.tern';
    return Endpoint.unix('$home/tern.sock');
  }

  /// Base URL for requests. Unix sockets use a placeholder host.
  Uri get base;

  HttpClient httpClient();

  String describe();
}

final class UnixEndpoint extends Endpoint {
  const UnixEndpoint(this.path);

  final String path;

  @override
  Uri get base => Uri.parse('http://tern');

  @override
  HttpClient httpClient() {
    final client = HttpClient();
    client.connectionFactory = (uri, proxyHost, proxyPort) {
      // The daemon may advertise a symlink to a shorter path in /tmp.
      final target = _resolve(path);
      return Socket.startConnect(
        InternetAddress(target, type: InternetAddressType.unix),
        0,
      );
    };
    return client;
  }

  static String _resolve(String path) {
    try {
      return File(path).resolveSymbolicLinksSync();
    } on FileSystemException {
      return path;
    }
  }

  @override
  String describe() => path;
}
