import 'dart:convert';
import 'dart:io';

void main() async {
  final server = await HttpServer.bind(InternetAddress.anyIPv4, 8080);

  server.listen((request) async {
    if (request.uri.path == '/db/auth') {
      final response = {
        'db_url': 'staging-db-lhr-08c0519209-simple-tail-globe.turso.io',
        'db_token':
            'eyJhbGciOiJFZERTQSIsInR5cCI6IkpXVCJ9.eyJhIjoicnciLCJleHAiOjE3NTQ2NzIwMDAsImlhdCI6MTc1NDU4NTYwMCwiaWQiOiJiMTZkZDkxMS05NmE4LTQzYTMtYTg3OC01ZGMzZjQ5MjVlZTkifQ.GrjdkI7SSLG3yTjpqFVzDGOdxs0PNlsvh3sVmkiQ-k0Rkvhv1h_-56tNmHLk_TYruW6vySmpr8UMhbKDN9tdBw',
      };

      request.response
        ..statusCode = HttpStatus.ok
        ..headers.contentType = ContentType.json
        ..write(json.encode(response))
        ..close();
      return;
    }

    // Handle the request
    if (request.method == 'GET') {
      request.response
        ..write('Hello from the proxy server!')
        ..close();
    } else {
      request.response
        ..statusCode = HttpStatus.methodNotAllowed
        ..write('Method not allowed')
        ..close();
    }
  });

  print(
      'Proxy server running on http://${server.address.address}:${server.port}');

  // Handle server shutdown gracefully
  ProcessSignal.sigterm.watch().listen((_) async {
    await server.close();
    print('Proxy server stopped.');
  });
}
