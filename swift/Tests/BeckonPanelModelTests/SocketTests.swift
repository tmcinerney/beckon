import XCTest
import Darwin
@testable import BeckonPanelModel

private final class UnixFixture: @unchecked Sendable {
    let path:String
    let listener:Int32
    let worker:Task<Void,Never>
    init(mode:String) throws {
        path="/tmp/bp-\(UUID().uuidString.prefix(12)).sock"
        let fd=socket(AF_UNIX,SOCK_STREAM,0);listener=fd
        guard fd>=0 else {throw PanelError.unavailable("Fixture socket")}
        var address=sockaddr_un();address.sun_family=sa_family_t(AF_UNIX);address.sun_len=UInt8(MemoryLayout<sockaddr_un>.size)
        let bytes=Array(path.utf8)+[0]
        withUnsafeMutableBytes(of:&address.sun_path){$0.copyBytes(from:bytes)}
        let bound=withUnsafePointer(to:&address){$0.withMemoryRebound(to:sockaddr.self,capacity:1){bind(fd,$0,socklen_t(MemoryLayout<sockaddr_un>.size))}}
        guard bound==0,listen(fd,1)==0 else {close(fd);throw PanelError.unavailable("Fixture bind")}
        worker=Task.detached {
            let client=accept(fd,nil,nil)
            guard client>=0 else{return}
            defer {close(client)}
            var yes:Int32=1;setsockopt(client,SOL_SOCKET,SO_NOSIGPIPE,&yes,socklen_t(MemoryLayout<Int32>.size))
            var received=Data(),buffer=[UInt8](repeating:0,count:4096)
            while !received.contains(10),received.count<16384 {
                let n=read(client,&buffer,buffer.count)
                guard n>0 else{return};received.append(contentsOf:buffer.prefix(n))
            }
            guard let request=(try? JSONSerialization.jsonObject(with:received)) as? [String:Any],
                  request["method"] as? String=="agent.list" else{return}
            if mode=="stall" {try? await Task.sleep(nanoseconds:600_000_000);return}
            var response:Data
            if mode=="cap" {response=Data(repeating:120,count:1_048_577)}
            else if mode=="partial" {response=Data("{\"id\":".utf8)}
            else {
                let id=mode=="wrong" ? "wrong" : request["id"] as? String ?? ""
                response=(try? JSONSerialization.data(withJSONObject:["id":id,"result":["agents":[]]])) ?? Data()
                response.append(10)
            }
            // Fragmented framing exercises partial reads/writes without a live service.
            var sent=0
            while sent<response.count {
                let n=response.withUnsafeBytes { p in
                    send(client,p.baseAddress!.advanced(by:sent),min(997,response.count-sent),0)
                }
                guard n>0 else{return};sent+=n
            }
        }
    }
    deinit {shutdown(listener,SHUT_RDWR);close(listener);unlink(path)}
}
final class SocketTests:XCTestCase {
    func testVerifiedReadOnlyRPCAndFragmentedReply() async throws {
        let server=try UnixFixture(mode:"ok")
        let result=try await SocketAgentSource().read(source:SourceKey(socket:server.path),budget:1)
        XCTAssertTrue(result.agents.isEmpty)
        await server.worker.value
    }
    func testWrongIDPartialJSONAndCapAreUnavailable() async throws {
        for mode in ["wrong","partial","cap"] {
            let server=try UnixFixture(mode:mode)
            do {_=try await SocketAgentSource().read(source:SourceKey(socket:server.path),budget:1);XCTFail("Accepted \(mode)")}
            catch {if mode=="cap" {XCTAssertEqual(error as? PanelError,.outputLimit)}}
            await server.worker.value
        }
    }
    func testSocketDeadlineAndCancellationAreBounded() async throws {
        let server=try UnixFixture(mode:"stall")
        let start=Date()
        do {_=try await SocketAgentSource().read(source:SourceKey(socket:server.path),budget:0.15);XCTFail("Ignored deadline")}
        catch {XCTAssertEqual(error as? PanelError,.timeout)}
        XCTAssertLessThan(Date().timeIntervalSince(start),0.5)
        await server.worker.value
        let cancelled=try UnixFixture(mode:"stall")
        let job=Task {try await SocketAgentSource().read(source:SourceKey(socket:cancelled.path),budget:1)}
        try await Task.sleep(nanoseconds:30_000_000);job.cancel()
        do {_=try await job.value;XCTFail("Ignored cancellation")}
        catch {XCTAssertEqual(error as? PanelError,.cancelled)}
        await cancelled.worker.value
    }
}
