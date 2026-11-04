import 'package:flutter/material.dart';

import '../state/store.dart';
import 'widgets.dart';

/// The avatar of whoever `authorId` is (agent profiles §2): an agent
/// profile's glyph and colour for `tern…` ids (a sub-agent's
/// `<agent>/<label>` gets its agent's), else one generated from `name`.
Widget authorAvatar(
  TernStore store, {
  required String authorId,
  required String name,
  double size = 20,
}) => switch (store.agentOfAuthor(authorId)) {
  final a? => AgentAvatar.of(a, size: size),
  null => AgentAvatar(name: name, size: size),
};

/// The avatar of the agent a stream or view belongs to: the open stream's,
/// else the default agent's as it's named now, else tern's generated one.
Widget currentAgentAvatar(TernStore store, {double size = 24}) =>
    switch (store.streamAgent ?? store.defaultAgent) {
      final a? => AgentAvatar.of(a, size: size),
      null => AgentAvatar(name: 'tern', size: size),
    };
