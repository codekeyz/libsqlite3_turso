import 'dart:io';

import 'package:sqlite3/sqlite3.dart';
import 'package:path/path.dart' as path;

// ignore: public_member_api_docs
void copyBinaryIfNecessary() {
  const libraryName = 'libsqlite3.so';
  final systemBinary = File('/lib/$libraryName')..createSync(recursive: true);

  final customBinary =
      File(path.join(Directory.current.path, 'target', 'debug', libraryName));
  customBinary.copySync(systemBinary.path);
}

void main() {
  copyBinaryIfNecessary();

  const commmands = [
    'CREATE TABLE IF NOT EXISTS users (id INTEGER PRIMARY KEY, name TEXT, email TEXT)',
    // "INSERT INTO users (name, email) VALUES ('Alice', 'alice@gmail.com')",
  ];

  final db = sqlite3.open('untrue-necklace');
  for (final command in commmands) db.execute(command);

  // fetch data
  final result = db.select("SELECT * FROM users");
  for (final row in result) {
    stdout.writeln(
      'Artist[id: ${row['id']}, name: ${row['name']}, email: ${row['email']}]',
    );
  }

  db.dispose();
}
