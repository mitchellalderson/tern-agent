import 'dart:io';

import 'package:crypto/crypto.dart';

/// Where a tern core's API lives, and how to reach it (deployment spec
/// §7.2's transports).
///
/// The local daemon listens on a Unix socket and trusts whoever can open
/// it. A remote core is reached over HTTPS with a device token, pinning
/// its self-signed certificate when the pairing link carried a
/// fingerprint. Everything above this (requests, SSE, models) is the same
/// for both.
sealed class Endpoint {
  const Endpoint();

  const factory Endpoint.unix(String path) = UnixEndpoint;
  const factory Endpoint.remote(
    Uri base, {
    required String token,
    String? pin,
  }) = RemoteEndpoint;

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

/// A core reached over the network with a device token.
///
/// With a [pin] (`sha256:` and 64 hex digits, from the pairing link), the
/// certificate must hash to it and nothing else is trusted, not even the
/// system's CAs. Without one, the certificate is verified normally (a core
/// using `tls = "files"`).
final class RemoteEndpoint extends Endpoint {
  const RemoteEndpoint(this.base, {required this.token, this.pin});

  @override
  final Uri base;
  final String token;
  final String? pin;

  @override
  Map<String, String> get headers => {'authorization': 'Bearer $token'};

  @override
  HttpClient httpClient() {
    final pin = this.pin;
    if (pin == null) return HttpClient();
    final want = parsePin(pin);
    // No trusted roots: every certificate reaches the callback, which
    // accepts only the pinned one.
    return HttpClient(context: SecurityContext(withTrustedRoots: false))
      ..badCertificateCallback = (cert, host, port) =>
          _sameBytes(sha256.convert(cert.der).bytes, want);
  }

  @override
  String describe() => base.toString();
}

/// The SHA-256 a pin names. Accepts `sha256:` + hex, upper or lower case,
/// with or without colons.
List<int> parsePin(String pin) {
  var hex = pin.startsWith('sha256:') ? pin.substring(7) : pin;
  hex = hex.replaceAll(':', '');
  if (hex.length != 64 || !RegExp(r'^[0-9a-fA-F]+$').hasMatch(hex)) {
    throw FormatException(
      'certificate pin must be sha256: and 64 hex digits',
      pin,
    );
  }
  return [
    for (var i = 0; i < 64; i += 2)
      int.parse(hex.substring(i, i + 2), radix: 16),
  ];
}

/// The pin for a DER certificate, as the core puts it in pairing links.
String fingerprint(List<int> der) =>
    'sha256:${sha256.convert(der).toString().toUpperCase()}';

bool _sameBytes(List<int> a, List<int> b) {
  if (a.length != b.length) return false;
  var diff = 0;
  for (var i = 0; i < a.length; i++) {
    diff |= a[i] ^ b[i];
  }
  return diff == 0;
}

/// Whether [host] is this computer: `localhost`, `*.localhost`, 127.0.0.0/8
/// or `::1`.
bool isLoopbackHost(String host) {
  final h = host.toLowerCase();
  if (h == 'localhost' || h.endsWith('.localhost')) return true;
  final ip = InternetAddress.tryParse(h);
  return ip != null && ip.isLoopback;
}

/// Why a tern server can't be at [url], in a sentence, or null when it can:
/// it must be `https` (the token travels with every request) unless it is
/// this computer, where `http` never leaves the machine.
String? remoteUrlProblem(Uri url) {
  if (url.host.isEmpty || (url.scheme != 'https' && url.scheme != 'http')) {
    return "That doesn't look like a server address.";
  }
  if (url.hasQuery || url.hasFragment || url.userInfo.isNotEmpty) {
    return 'Use just the server address, like https://home.example.net:7420, '
        'with no login, query or #.';
  }
  if (url.scheme == 'http' && !isLoopbackHost(url.host)) {
    return 'Use https:// for a server on another computer. tern sends a '
        'secret key with every request, and http would send it unprotected.';
  }
  return null;
}

/// A `tern://pair?u=…&c=…&fp=…&n=…` link from `tern devices pair` or the
/// desktop app's Add device (deployment spec §6.2).
class PairingLink {
  const PairingLink({
    required this.url,
    required this.code,
    this.fingerprint,
    this.name,
  });

  factory PairingLink.parse(String link) {
    final uri = Uri.parse(link.trim());
    if (uri.scheme != 'tern' || uri.host != 'pair') {
      throw FormatException('a pairing link starts with tern://pair?', link);
    }
    final q = uri.queryParameters;
    final url = q['u'];
    final code = q['c'];
    if (url == null || code == null) {
      throw FormatException(
        'the pairing link needs a core URL and a code',
        link,
      );
    }
    final base = Uri.parse(url);
    if (base.scheme != 'https' && base.scheme != 'http') {
      throw FormatException('the core URL must be https', url);
    }
    final fp = q['fp'];
    if (fp != null) parsePin(fp);
    return PairingLink(url: base, code: code, fingerprint: fp, name: q['n']);
  }

  final Uri url;
  final String code;
  final String? fingerprint;
  final String? name;
}
