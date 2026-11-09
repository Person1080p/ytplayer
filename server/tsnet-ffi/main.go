// Package main is built as a C archive (go build -buildmode=c-archive) and linked into
// ytp-server, so the server can join your tailnet by itself — no Tailscale install on
// the host. It wraps tailscale.com/tsnet behind a tiny C API used from Rust
// (server/src/services/tailscale.rs):
//
//	ytp_ts_start(dir, hostname, authkey, controlURL) -> 0 | -1
//	ytp_ts_status()            -> malloc'd JSON (ipnstate.Status) or NULL; free with ytp_ts_free
//	ytp_ts_login()             -> 0 | -1   (request an interactive sign-in URL)
//	ytp_ts_forward(addr, target, tls) -> 0 | -1
//	    listen on the tailnet at addr (":80", ":443") and proxy each connection to the
//	    local TCP target ("127.0.0.1:8080"); with tls=1, terminate HTTPS using the node's
//	    *.ts.net certificate (needs HTTPS enabled for the tailnet).
//	ytp_ts_last_error()        -> malloc'd message for the last failure
//	ytp_ts_free(p)
package main

/*
#include <stdlib.h>
*/
import "C"

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log"
	"net"
	"os"
	"strconv"
	"sync"
	"time"
	"unsafe"

	"tailscale.com/tsnet"
)

var (
	mu      sync.Mutex
	srv     *tsnet.Server
	lastErr string

	logMu       sync.Mutex
	lastUserMsg string
)

func main() {} // required for -buildmode=c-archive

func fail(err error) C.int {
	mu.Lock()
	lastErr = err.Error()
	mu.Unlock()
	return -1
}

func server() (*tsnet.Server, error) {
	mu.Lock()
	defer mu.Unlock()
	if srv == nil {
		return nil, errors.New("tailscale node not started")
	}
	return srv, nil
}

//export ytp_ts_start
func ytp_ts_start(dir, hostname, authkey, controlURL *C.char) C.int {
	mu.Lock()
	if srv != nil {
		mu.Unlock()
		return 0
	}
	verbose := os.Getenv("YTP_TS_VERBOSE") != ""
	// A fixed UDP port (TS_PORT) can be published from a container or forwarded on the router, so
	// devices reach the node directly instead of through Tailscale's relays. 0: any free port.
	port, _ := strconv.ParseUint(os.Getenv("TS_PORT"), 10, 16)
	s := &tsnet.Server{
		Port:       uint16(port),
		Dir:        C.GoString(dir),
		Hostname:   C.GoString(hostname),
		AuthKey:    C.GoString(authkey),
		ControlURL: C.GoString(controlURL),
		Logf: func(format string, args ...any) {
			if verbose {
				log.Printf("tsnet: "+format, args...)
			}
		},
		// User-facing messages (e.g. the sign-in link) go to stderr, each distinct message once.
		UserLogf: func(format string, args ...any) {
			msg := fmt.Sprintf(format, args...)
			logMu.Lock()
			defer logMu.Unlock()
			if msg != lastUserMsg {
				lastUserMsg = msg
				log.Printf("tailscale: %s", msg)
			}
		},
	}
	mu.Unlock()

	// Start doesn't wait for login: the sign-in link shows up in the status.
	if err := s.Start(); err != nil {
		return fail(err)
	}
	mu.Lock()
	srv = s
	mu.Unlock()
	return 0
}

//export ytp_ts_status
func ytp_ts_status() *C.char {
	s, err := server()
	if err != nil {
		fail(err)
		return nil
	}
	lc, err := s.LocalClient()
	if err != nil {
		fail(err)
		return nil
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	st, err := lc.StatusWithoutPeers(ctx)
	if err != nil {
		fail(err)
		return nil
	}
	b, err := json.Marshal(st)
	if err != nil {
		fail(err)
		return nil
	}
	return C.CString(string(b))
}

//export ytp_ts_login
func ytp_ts_login() C.int {
	s, err := server()
	if err != nil {
		return fail(err)
	}
	lc, err := s.LocalClient()
	if err != nil {
		return fail(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	if err := lc.StartLoginInteractive(ctx); err != nil {
		return fail(err)
	}
	return 0
}

//export ytp_ts_forward
func ytp_ts_forward(addr, target *C.char, useTLS C.int) C.int {
	s, err := server()
	if err != nil {
		return fail(err)
	}
	a, tgt := C.GoString(addr), C.GoString(target)
	var ln net.Listener
	if useTLS != 0 {
		ln, err = s.ListenTLS("tcp", a)
	} else {
		ln, err = s.Listen("tcp", a)
	}
	if err != nil {
		return fail(err)
	}
	go func() {
		for {
			c, err := ln.Accept()
			if err != nil {
				log.Printf("tailscale: listener %s stopped: %v", a, err)
				return
			}
			go proxy(c, tgt)
		}
	}()
	return 0
}

func proxy(c net.Conn, target string) {
	defer c.Close()
	d, err := net.DialTimeout("tcp", target, 10*time.Second)
	if err != nil {
		return
	}
	defer d.Close()
	done := make(chan struct{}, 2)
	go func() { io.Copy(d, c); done <- struct{}{} }()
	go func() { io.Copy(c, d); done <- struct{}{} }()
	<-done // either side closing ends the session
}

//export ytp_ts_last_error
func ytp_ts_last_error() *C.char {
	mu.Lock()
	defer mu.Unlock()
	return C.CString(lastErr)
}

//export ytp_ts_free
func ytp_ts_free(p *C.char) {
	C.free(unsafe.Pointer(p))
}
