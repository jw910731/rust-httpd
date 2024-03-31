# Rust HTTPd (Insomnia Server)
A rust http server that leverages async/await and task pool to get great performance.
This project is for learning perpose and not intended for production use.

## Features
- Echo server
The POST `/echo` endpoint provide a http echo server.
- Static file server
The default endpoints maps to the `/static` directory under the project dir. Serves large file in rapid speed, no cache is introduced since I didn't come up with a cache policy.


## TODO and Future work
These features will be supported when it's snowy in the summer.
- [] File cache mechanism without blown up the memory space.
- [] TLS support
- [] Homemade QUIC support

## About The Name
The server is named "Insomnia Server" because it was written when I had a insomnia and I thankfully get into sleep after working on it.