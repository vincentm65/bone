"""Exercise the real daemon using the local mock_provider.py fixture."""
import json
import socket
import time


class Client:
    def __init__(self):
        self.socket = socket.create_connection(('127.0.0.1', 17878), 10)
        self.socket.settimeout(15)
        self.reader = self.socket.makefile()
        self.loaded = self.until('conversation_loaded')

    def send(self, value):
        self.socket.sendall((json.dumps(value) + '\n').encode())

    def until(self, kind):
        while True:
            line = self.reader.readline()
            assert line, 'unexpected EOF'
            event = json.loads(line)
            if isinstance(event, dict) and kind in event:
                return event[kind]

    def close(self):
        self.reader.close()
        self.socket.close()


def run():
    a = Client()
    a.send('new_conversation')
    first = a.until('conversation_loaded')['snapshot']['conversation_id']
    start = time.monotonic()
    a.send({'submit_prompt': {'request_id': 1, 'text': 'Desktop smoke', 'images': []}})
    a.until('started')
    assert a.until('text_delta')['text'] == 'Desktop '
    assert a.until('finished')['content'] == 'Desktop local fixture response.'
    a.until('turn_completed')
    print(f'PASS real-daemon stream/finish: {time.monotonic()-start:.3f}s')
    a.close()
    a = Client()
    a.send({'load_conversation': {'id': first}})
    loaded = a.until('conversation_loaded')
    assert loaded['snapshot']['conversation_id'] == first
    assert any(m['content'] == 'Desktop local fixture response.' for m in loaded['messages'])
    print('PASS reconnect/load persisted history')
    b = Client()
    b.send('new_conversation')
    second = b.until('conversation_loaded')['snapshot']['conversation_id']
    assert first != second
    a.send({'submit_prompt': {'request_id': 2, 'text': 'slow cancel test', 'images': []}})
    a.until('started')
    b.send({'submit_prompt': {'request_id': 3, 'text': 'independent test', 'images': []}})
    b.until('started')
    a.send('cancel')
    a.until('turn_completed')
    assert b.until('finished')['content'] == 'Desktop local fixture response.'
    b.until('turn_completed')
    print('PASS concurrent actors and scoped cancellation')
    a.close()
    b.close()
    c = Client()
    c.close()
    print('PASS daemon survives client shutdown')


if __name__ == '__main__':
    run()
