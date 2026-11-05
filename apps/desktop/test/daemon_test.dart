import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:tern_desktop/state/daemon.dart';

void main() {
  test('bundledTern points into the tray helper of an app bundle', () {
    expect(
      bundledTern('/Applications/tern.app/Contents/MacOS/tern'),
      '/Applications/tern.app/Contents/Library/LoginItems/tern Tray.app/Contents/MacOS/tern-tray',
    );
    expect(
      bundledTern('/usr/lib/tern-desktop/tern_desktop'),
      Platform.isLinux ? '/usr/lib/tern-desktop/tern' : isNull,
    );
  });

  test('findTern prefers TERN_BIN, then the bundled binary, then PATH', () {
    final dir = Directory.systemTemp.createTempSync('tern_find');
    addTearDown(() => dir.deleteSync(recursive: true));
    File touch(String path) =>
        File('${dir.path}/$path')..createSync(recursive: true);
    final exe = '${dir.path}/tern.app/Contents/MacOS/tern';
    final bundled = touch(
      'tern.app/Contents/Library/LoginItems/tern Tray.app/Contents/MacOS/tern-tray',
    ).path;
    final onPath = touch('bin/tern').path;
    final explicit = touch('custom/tern').path;
    final env = {'HOME': dir.path, 'PATH': '${dir.path}/bin'};

    expect(findTern({...env, 'TERN_BIN': explicit}, exe), explicit);
    expect(findTern(env, exe), bundled);
    expect(findTern(env, '${dir.path}/elsewhere/tern_desktop'), onPath);
  });
}
