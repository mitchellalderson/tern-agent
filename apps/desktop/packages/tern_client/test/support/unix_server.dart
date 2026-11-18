import 'dart:io';

import 'package:tern_client/tern_client.dart';
import 'package:test/test.dart';

/// An HTTP server on a throwaway Unix socket, and the endpoint that reaches
/// it. The socket's folder is removed when the test ends.
Future<(HttpServer, Endpoint)> bindUnixServer() async {
  final dir = Directory.systemTemp.createTempSync('tern-sock');
  addTearDown(() => dir.deleteSync(recursive: true));
  final path = '${dir.path}/s.sock';
  final server = HttpServer.listenOn(
    await ServerSocket.bind(
      InternetAddress(path, type: InternetAddressType.unix),
      0,
    ),
  );
  return (server, Endpoint.unix(path));
}
