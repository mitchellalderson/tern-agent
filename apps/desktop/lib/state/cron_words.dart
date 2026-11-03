/// Common cron expressions in words ("Weekdays at 8:00 AM"); anything else
/// comes back as the expression itself.
String cronInWords(String cron) {
  final f = cron.trim().split(RegExp(r'\s+'));
  if (f.length != 5) return cron;
  final [min, hour, dom, mon, dow] = f;
  if (dom != '*' || mon != '*') return cron;
  if (min.startsWith('*/') && hour == '*' && dow == '*') {
    return 'Every ${min.substring(2)} minutes';
  }
  final m = int.tryParse(min);
  if (m == null) return cron;
  if (hour == '*' && dow == '*') {
    return m == 0 ? 'Hourly' : 'Hourly at :${min.padLeft(2, '0')}';
  }
  final h = int.tryParse(hour);
  if (h == null) return cron;
  final at = 'at ${clock(h, m)}';
  final days = switch (dow.toLowerCase()) {
    '*' => 'Daily',
    '1-5' || 'mon-fri' => 'Weekdays',
    '0,6' || '6,0' || 'sat,sun' || '6-7' => 'Weekends',
    final d when _day.containsKey(d) => '${_day[d]}s',
    _ => null,
  };
  return days == null ? cron : '$days $at';
}

const _day = {
  '0': 'Sunday',
  '7': 'Sunday',
  'sun': 'Sunday',
  '1': 'Monday',
  'mon': 'Monday',
  '2': 'Tuesday',
  'tue': 'Tuesday',
  '3': 'Wednesday',
  'wed': 'Wednesday',
  '4': 'Thursday',
  'thu': 'Thursday',
  '5': 'Friday',
  'fri': 'Friday',
  '6': 'Saturday',
  'sat': 'Saturday',
};

String clock(int h, int m) =>
    '${h % 12 == 0 ? 12 : h % 12}:${m.toString().padLeft(2, '0')} ${h < 12 ? 'AM' : 'PM'}';

/// "Today 8:00 AM", "Tomorrow 9:30 PM", "Fri 8:00 AM", "Dec 4 8:00 AM".
String whenInWords(DateTime t, [DateTime? now]) {
  now ??= DateTime.now();
  final today = DateTime(now.year, now.month, now.day);
  final day = DateTime(t.year, t.month, t.day);
  final diff = day.difference(today).inDays;
  final c = clock(t.hour, t.minute);
  if (diff == 0) return 'Today $c';
  if (diff == 1) return 'Tomorrow $c';
  if (diff > 1 && diff < 7) {
    return '${const ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'][t.weekday - 1]} $c';
  }
  const months = [
    'Jan',
    'Feb',
    'Mar',
    'Apr',
    'May',
    'Jun',
    'Jul',
    'Aug',
    'Sep',
    'Oct',
    'Nov',
    'Dec',
  ];
  return '${months[t.month - 1]} ${t.day} $c';
}
