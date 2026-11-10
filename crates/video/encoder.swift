// lapsewerk-encode: writes the frames it reads on stdin into a QuickTime movie with AVFoundation.
//
// Usage: lapsewerk-encode --out FILE --width W --height H --fps NUM[/DEN]
//                         --codec prores422hq|prores422|prores4444|hevc [--bitrate BPS]
//                         [--transfer srgb|709|pq|hlg]
//                         [--mastering-display HEX48 --content-light-level HEX8]   (PQ: HDR10)
//                         [--timecode-frame N]   (a timecode track starting at frame N)
//                         [--drop-frame 1]       (the timecode counts drop-frame: 29.97, 59.94)
//
// Input on stdin, message by message:
//   'F' + one frame: W × H pixels of 16-bit big-endian ARGB ("b64a", 8 bytes a pixel, rows packed)
//   'E' + the number of frames sent (u64, big-endian): the end; the movie is finished.
// Input that ends without the end mark (Lapsewerk was killed: out of memory, kill -9) cancels the
// movie and deletes FILE, so no complete-looking movie is left behind. SDR movies (srgb, 709) are
// tagged Rec. 709 primaries and matrix with the sRGB or Rec. 709 transfer curve; HDR movies (pq,
// hlg: the values already carry the signal) Rec. 2020 primaries and matrix with PQ or HLG. Writes nothing but FILE; exit
// status 0 only when every frame was written and the movie finished. Lapsewerk starts it with fixed
// arguments and writes to a temporary name.

import AVFoundation
import CoreVideo
import Foundation
import VideoToolbox

struct Failure: Error { let message: String }

func fail(_ message: String) -> Never {
    FileHandle.standardError.write(("lapsewerk-encode: " + message + "\n").data(using: .utf8)!)
    exit(1)
}

struct Options {
    var out = ""
    var width = 0
    var height = 0
    var fpsNum: Int32 = 0
    var fpsDen: Int32 = 1
    var codec = ""
    var bitrate = 0
    var transfer = "srgb"
    var mastering = ""
    var lightLevel = ""
    var timecodeFrame = -1
    var dropFrame = false
}

/// Bytes from hexadecimal text; nil unless it is exactly `count` bytes.
func bytes(hex: String, count: Int) -> Data? {
    let chars = Array(hex.utf8)
    guard chars.count == count * 2 else { return nil }
    var out = Data(capacity: count)
    for i in stride(from: 0, to: chars.count, by: 2) {
        guard let b = UInt8(String(decoding: chars[i..<i + 2], as: UTF8.self), radix: 16) else { return nil }
        out.append(b)
    }
    return out
}

func parseOptions() -> Options {
    var o = Options()
    var args = CommandLine.arguments.dropFirst().makeIterator()
    while let a = args.next() {
        guard let v = args.next() else { fail("\(a) needs a value") }
        switch a {
        case "--out": o.out = v
        case "--width": o.width = Int(v) ?? 0
        case "--height": o.height = Int(v) ?? 0
        case "--fps":
            let parts = v.split(separator: "/")
            o.fpsNum = Int32(parts.first ?? "") ?? 0
            o.fpsDen = parts.count > 1 ? (Int32(parts[1]) ?? 0) : 1
        case "--codec": o.codec = v
        case "--bitrate": o.bitrate = Int(v) ?? -1
        case "--transfer": o.transfer = v
        case "--mastering-display": o.mastering = v
        case "--content-light-level": o.lightLevel = v
        case "--timecode-frame": o.timecodeFrame = Int(v) ?? -2
        case "--drop-frame": o.dropFrame = v == "1"
        default: fail("unknown option \(a)")
        }
    }
    guard !o.out.isEmpty else { fail("--out missing") }
    // Even sizes: 4:2:0 and 4:2:2 codecs need them.
    guard (16...16384).contains(o.width), (16...16384).contains(o.height), o.width % 2 == 0, o.height % 2 == 0
    else { fail("width and height must be even, 16…16384") }
    guard (1...240_000).contains(o.fpsNum), (1...10_000).contains(o.fpsDen) else { fail("bad --fps") }
    guard o.bitrate >= 0 else { fail("bad --bitrate") }
    guard o.timecodeFrame >= -1, o.timecodeFrame < Int(Int32.max) else { fail("bad --timecode-frame") }
    guard ["srgb", "709", "pq", "hlg"].contains(o.transfer) else { fail("--transfer is srgb, 709, pq or hlg") }
    return o
}

let o = parseOptions()

let codec: AVVideoCodecType
switch o.codec {
case "prores422hq": codec = .proRes422HQ
case "prores422": codec = .proRes422
case "prores4444": codec = .proRes4444
case "hevc": codec = .hevc
default: fail("unknown codec \(o.codec)")
}

let hdr = o.transfer == "pq" || o.transfer == "hlg"
let transfer: String
let bufferTransfer: CFString
switch o.transfer {
case "srgb": (transfer, bufferTransfer) = (AVVideoTransferFunction_IEC_sRGB, kCVImageBufferTransferFunction_sRGB)
case "pq": (transfer, bufferTransfer) = (AVVideoTransferFunction_SMPTE_ST_2084_PQ, kCVImageBufferTransferFunction_SMPTE_ST_2084_PQ)
case "hlg": (transfer, bufferTransfer) = (AVVideoTransferFunction_ITU_R_2100_HLG, kCVImageBufferTransferFunction_ITU_R_2100_HLG)
default: (transfer, bufferTransfer) = (AVVideoTransferFunction_ITU_R_709_2, kCVImageBufferTransferFunction_ITU_R_709_2)
}
let primaries = hdr ? AVVideoColorPrimaries_ITU_R_2020 : AVVideoColorPrimaries_ITU_R_709_2
let matrix = hdr ? AVVideoYCbCrMatrix_ITU_R_2020 : AVVideoYCbCrMatrix_ITU_R_709_2
let bufferPrimaries = hdr ? kCVImageBufferColorPrimaries_ITU_R_2020 : kCVImageBufferColorPrimaries_ITU_R_709_2
let bufferMatrix = hdr ? kCVImageBufferYCbCrMatrix_ITU_R_2020 : kCVImageBufferYCbCrMatrix_ITU_R_709_2
let colour: [String: Any] = [
    AVVideoColorPrimariesKey: primaries,
    AVVideoTransferFunctionKey: transfer,
    AVVideoYCbCrMatrixKey: matrix,
]
var settings: [String: Any] = [
    AVVideoCodecKey: codec,
    AVVideoWidthKey: o.width,
    AVVideoHeightKey: o.height,
    AVVideoColorPropertiesKey: colour,
]
// HDR10 static metadata (SMPTE ST 2086, CTA-861.3), made by Lapsewerk: PQ only, any codec. HEVC
// carries it in the stream too; every codec gets it on the frames, so the movie's description holds
// it ('mdcv', 'clli').
var masteringData: Data? = nil
var lightLevelData: Data? = nil
if !o.mastering.isEmpty || !o.lightLevel.isEmpty {
    guard o.transfer == "pq",
          let md = bytes(hex: o.mastering, count: 24),
          let cll = bytes(hex: o.lightLevel, count: 4)
    else { fail("HDR10 metadata: PQ only, 24 + 4 bytes in hexadecimal") }
    masteringData = md
    lightLevelData = cll
}
if codec == .hevc {
    let fps = Double(o.fpsNum) / Double(o.fpsDen)
    // About 0.2 bits a pixel: visually clean for time-lapse at any size.
    let bitrate = o.bitrate > 0 ? o.bitrate : Int(Double(o.width * o.height) * fps * 0.2)
    var compression: [String: Any] = [
        AVVideoAverageBitRateKey: bitrate,
        AVVideoProfileLevelKey: kVTProfileLevel_HEVC_Main10_AutoLevel as String,
        AVVideoExpectedSourceFrameRateKey: fps,
        AVVideoAllowFrameReorderingKey: true,
    ]
    if let md = masteringData, let cll = lightLevelData {
        compression[kVTCompressionPropertyKey_MasteringDisplayColorVolume as String] = md
        compression[kVTCompressionPropertyKey_ContentLightLevelInfo as String] = cll
    }
    settings[AVVideoCompressionPropertiesKey] = compression
} else if let md = masteringData, let cll = lightLevelData {
    // ProRes: the HDR10 metadata as compression properties too (frame attachments alone do not
    // reach a ProRes movie's description). A system that refuses them gets the movie without.
    var withHdr = settings
    withHdr[AVVideoCompressionPropertiesKey] = [
        kVTCompressionPropertyKey_MasteringDisplayColorVolume as String: md,
        kVTCompressionPropertyKey_ContentLightLevelInfo as String: cll,
    ]
    let probe = try? AVAssetWriter(outputURL: URL(fileURLWithPath: o.out + ".probe"), fileType: .mov)
    if probe?.canApply(outputSettings: withHdr, forMediaType: .video) == true {
        settings = withHdr
    } else {
        FileHandle.standardError.write("lapsewerk-encode: ProRes HDR10 metadata not supported here; written without\n".data(using: .utf8)!)
    }
    try? FileManager.default.removeItem(atPath: o.out + ".probe")
}

let url = URL(fileURLWithPath: o.out)
let writer: AVAssetWriter
do {
    writer = try AVAssetWriter(outputURL: url, fileType: .mov)
} catch {
    fail("cannot create \(o.out): \(error.localizedDescription)")
}
guard writer.canApply(outputSettings: settings, forMediaType: .video) else { fail("codec settings not supported") }
let input = AVAssetWriterInput(mediaType: .video, outputSettings: settings)
input.expectsMediaDataInRealTime = false
// The track counts in the frame rate's own units (30000 for 29.97): with the default of 600 an NTSC
// frame (20.02 ticks) is rounded and every 25th frame lasts a tick longer, a variable frame rate.
input.mediaTimeScale = o.fpsNum
writer.movieTimeScale = o.fpsNum
let adaptor = AVAssetWriterInputPixelBufferAdaptor(
    assetWriterInput: input,
    sourcePixelBufferAttributes: [
        kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_64ARGB,
        kCVPixelBufferWidthKey as String: o.width,
        kCVPixelBufferHeightKey as String: o.height,
    ])
guard writer.canAdd(input) else { fail("cannot add the video track") }
writer.add(input)

// The timecode track (QuickTime 'tmcd'): one sample, the start frame, counted at the whole-frame
// rate; with --drop-frame flagged drop-frame (the sample is then the count of frames, which a
// reader shows as HH:MM:SS;FF). Written first and finished at once, so it never holds up the video.
var timecodeInput: AVAssetWriterInput? = nil
let quanta = UInt32((Double(o.fpsNum) / Double(o.fpsDen)).rounded())
if o.timecodeFrame >= 0 {
    var desc: CMTimeCodeFormatDescription?
    let status = CMTimeCodeFormatDescriptionCreate(
        allocator: kCFAllocatorDefault,
        timeCodeFormatType: kCMTimeCodeFormatType_TimeCode32,
        frameDuration: CMTime(value: Int64(o.fpsDen), timescale: o.fpsNum),
        frameQuanta: quanta,
        flags: kCMTimeCodeFlag_24HourMax | (o.dropFrame ? kCMTimeCodeFlag_DropFrame : 0),
        extensions: nil,
        formatDescriptionOut: &desc)
    guard status == noErr, let desc else { fail("timecode format (\(status))") }
    let tc = AVAssetWriterInput(mediaType: .timecode, outputSettings: nil, sourceFormatHint: desc)
    tc.expectsMediaDataInRealTime = false
    tc.mediaTimeScale = o.fpsNum
    guard writer.canAdd(tc) else { fail("cannot add the timecode track") }
    writer.add(tc)
    input.addTrackAssociation(withTrackOf: tc, type: AVAssetTrack.AssociationType.timecode.rawValue)
    timecodeInput = tc
}
guard writer.startWriting() else { fail("cannot start: \(writer.error?.localizedDescription ?? "?")") }
writer.startSession(atSourceTime: .zero)

if let tc = timecodeInput {
    var number = Int32(o.timecodeFrame).bigEndian
    var block: CMBlockBuffer?
    guard CMBlockBufferCreateWithMemoryBlock(
        allocator: kCFAllocatorDefault, memoryBlock: nil, blockLength: 4, blockAllocator: kCFAllocatorDefault,
        customBlockSource: nil, offsetToData: 0, dataLength: 4, flags: kCMBlockBufferAssureMemoryNowFlag,
        blockBufferOut: &block) == noErr, let block,
        CMBlockBufferReplaceDataBytes(with: &number, blockBuffer: block, offsetIntoDestination: 0, dataLength: 4) == noErr
    else { fail("timecode sample") }
    var timing = CMSampleTimingInfo(
        duration: CMTime(value: Int64(o.fpsDen), timescale: o.fpsNum), presentationTimeStamp: .zero,
        decodeTimeStamp: .invalid)
    var size = 4
    var sample: CMSampleBuffer?
    guard CMSampleBufferCreate(
        allocator: kCFAllocatorDefault, dataBuffer: block, dataReady: true, makeDataReadyCallback: nil,
        refcon: nil, formatDescription: tc.sourceFormatHint, sampleCount: 1, sampleTimingEntryCount: 1,
        sampleTimingArray: &timing, sampleSizeEntryCount: 1, sampleSizeArray: &size,
        sampleBufferOut: &sample) == noErr, let sample
    else { fail("timecode sample") }
    while !tc.isReadyForMoreMediaData { usleep(1000) }
    guard tc.append(sample) else { fail("timecode: \(writer.error?.localizedDescription ?? "?")") }
    tc.markAsFinished()
}

let rowBytes = o.width * 8
let frameBytes = rowBytes * o.height
var frame: Int64 = 0

/// Stops without a movie: cancels writing and deletes FILE.
func abandon(_ message: String) -> Never {
    writer.cancelWriting()
    try? FileManager.default.removeItem(at: url)
    fail(message)
}

// One buffer for every frame, read into with read(2). FileHandle's reads kept each frame's bytes
// (66 MB at 2160 × 3840) until the end, autorelease pool or not: a long 4K movie grew by a frame a
// second and was killed by the system for memory.
let inbuf = UnsafeMutableRawPointer.allocate(byteCount: max(frameBytes, 8), alignment: 16)

/// Reads exactly `count` bytes into `inbuf`; false when the input ends first.
func readBytes(_ count: Int) -> Bool {
    var got = 0
    while got < count {
        let r = read(0, inbuf + got, count - got)
        if r < 0 {
            if errno == EINTR { continue }
            abandon("read: error \(errno)")
        }
        if r == 0 { return false }
        got += r
    }
    return true
}

/// Reads the next frame into `inbuf`; false at the end mark (after checking the count); abandons
/// the movie when the input ends without one.
func readFrame(_ count: Int) -> Bool {
    guard readBytes(1) else { abandon("input ended without the end mark (Lapsewerk stopped)") }
    let tag = inbuf.load(as: UInt8.self)
    switch tag {
    case UInt8(ascii: "F"):
        guard readBytes(count) else { abandon("input ended inside frame \(frame)") }
        return true
    case UInt8(ascii: "E"):
        guard readBytes(8) else { abandon("input ended inside the end mark") }
        var sent: UInt64 = 0
        for i in 0..<8 { sent = (sent << 8) | UInt64(inbuf.load(fromByteOffset: i, as: UInt8.self)) }
        guard sent == UInt64(frame) else { abandon("end mark says \(sent) frames, \(frame) arrived") }
        return false
    default:
        abandon("unexpected input (\(tag))")
    }
}

// Each frame in its own autorelease pool: what AVFoundation hands back goes with the frame.
while autoreleasepool(invoking: { () -> Bool in
    guard readFrame(frameBytes) else { return false }
    while !input.isReadyForMoreMediaData {
        if writer.status == .failed { fail("encoder: \(writer.error?.localizedDescription ?? "?")") }
        usleep(1000)
    }
    guard let pool = adaptor.pixelBufferPool else { fail("no pixel buffer pool") }
    var buffer: CVPixelBuffer?
    guard CVPixelBufferPoolCreatePixelBuffer(nil, pool, &buffer) == kCVReturnSuccess, let buffer else {
        fail("cannot allocate a frame")
    }
    CVBufferSetAttachment(buffer, kCVImageBufferColorPrimariesKey, bufferPrimaries, .shouldPropagate)
    CVBufferSetAttachment(buffer, kCVImageBufferYCbCrMatrixKey, bufferMatrix, .shouldPropagate)
    CVBufferSetAttachment(buffer, kCVImageBufferTransferFunctionKey, bufferTransfer, .shouldPropagate)
    if let md = masteringData, let cll = lightLevelData {
        CVBufferSetAttachment(buffer, kCVImageBufferMasteringDisplayColorVolumeKey, md as CFData, .shouldPropagate)
        CVBufferSetAttachment(buffer, kCVImageBufferContentLightLevelInfoKey, cll as CFData, .shouldPropagate)
    }
    CVPixelBufferLockBaseAddress(buffer, [])
    guard let base = CVPixelBufferGetBaseAddress(buffer) else { fail("no frame memory") }
    let stride = CVPixelBufferGetBytesPerRow(buffer)
    for y in 0..<o.height {
        (base + y * stride).copyMemory(from: inbuf + y * rowBytes, byteCount: rowBytes)
    }
    CVPixelBufferUnlockBaseAddress(buffer, [])
    let time = CMTime(value: frame * Int64(o.fpsDen), timescale: o.fpsNum)
    guard adaptor.append(buffer, withPresentationTime: time) else {
        fail("frame \(frame): \(writer.error?.localizedDescription ?? "?")")
    }
    frame += 1
    return true
}) {}

guard frame > 0 else { abandon("no frames") }
input.markAsFinished()
let done = DispatchSemaphore(value: 0)
writer.finishWriting { done.signal() }
done.wait()
guard writer.status == .completed else { fail("finishing: \(writer.error?.localizedDescription ?? "?")") }
print("frames \(frame)")
