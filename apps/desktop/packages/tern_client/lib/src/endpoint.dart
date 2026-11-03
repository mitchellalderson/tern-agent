import 'dart:io';

/// Where the daemon's API lives.
///
/// The local daemon listens on a Unix socket. [Endpoint.tcp] is for a remote
/// or hosted core reached over the network with a bearer token; the daemon
/// doesn't serve TCP yet, but clients are written against this so they
/// don't assume a socket.
sealed class Endpoint {
  const Endpoint();

  const factory Endpoint.unix(String path) = UnixEndpoint;
  const factory Endpoint.tcp(Uri base, {String? token}) = TcpEndpoint;

  /// The local daemon's socket: `$TERN_HOME/tern.sock`, else `~/.tern/tern.sock`.
  static Endpoint local([Map<String, String>? env]) {
    env ??= Platform.environment;
    final home = env['TERN_HOME'] ?? '${env['HOME'] ?? ''}/.tern';
    return Endpoint.unix('$home/tern.sock');
  }

  /// Base URL for requests. Unix sockets use a placeholder host.
  Uri get base;

  /// Extra headers sent with every request.
  Map<String, String> get headers => const {};

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

final class TcpEndpoint extends Endpoint {
  const TcpEndpoint(this.base, {this.token});

  @override
  final Uri base;
  final String? token;

  @override
  Map<String, String> get headers =>
      token == null ? const {} : {'authorization': 'Bearer $token'};

  @override
  HttpClient httpClient() => HttpClient();

  @override
  String describe() => base.toString();
}
