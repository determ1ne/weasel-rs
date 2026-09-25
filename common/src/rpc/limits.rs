//! RPC 边界限制与运行时调优参数。

/// 跨进程消息的字段级协议限制。
pub(super) mod protocol {
    /// 请求 ID 为零表示无需响应的事件。
    pub const EVENT_REQUEST_ID: u64 = 0;
    /// 正常请求使用的首个 ID；后续 ID 单调递增。
    pub const FIRST_REQUEST_ID: u64 = 1;

    // 字符串上限均按 UTF-8 字节数计算，而不是 Unicode 字符数。
    pub const MAX_NOTIFICATION_SOURCE_BYTES: usize = 64;
    pub const MAX_NOTIFICATION_CODE_BYTES: usize = 128;
    pub const MAX_NOTIFICATION_TITLE_BYTES: usize = 512;
    pub const MAX_NOTIFICATION_MESSAGE_BYTES: usize = 2 * 1024;
    pub const MAX_NOTIFICATION_DETAILS_BYTES: usize = 16 * 1024;

    /// 引擎原始编码允许占用的最大 UTF-8 字节数。
    pub const MAX_RAW_INPUT_BYTES: usize = 64 * 1024;
    /// 单个输入状态最多携带的候选项数量。
    pub const MAX_CANDIDATES_PER_PAGE: usize = 256;
}

/// Named Pipe 任务的容量和截止时间。
pub(super) mod runtime {
    use std::time::Duration;

    /// 管道实例暂时繁忙时，两次连接尝试之间的退避时间。
    pub const CONNECT_RETRY_DELAY: Duration = Duration::from_millis(10);
    /// 双方首个 hello 帧允许等待的时间。
    pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
    /// 单帧写入允许占用写任务的最长时间。
    pub const FRAME_WRITE_TIMEOUT: Duration = Duration::from_secs(2);
    /// 普通 request/response 往返的内部截止时间。
    pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
    /// 服务端等待本地 writer 报告写入结果的时间；略长于实际帧写入期限。
    pub const SEND_ACK_TIMEOUT: Duration = Duration::from_secs(3);

    /// 客户端发出的请求与事件 FIFO。
    pub const CLIENT_OUTBOUND_CAPACITY: usize = 32;
    /// 对端响应和事件的统一订阅缓冲区。
    pub const CLIENT_INCOMING_CAPACITY: usize = 64;
    /// 单条客户端连接允许同时等待响应的请求数量。
    pub const MAX_PENDING_REQUESTS: usize = 64;

    /// 服务端交给业务层处理的有序消息容量。
    pub const SERVER_INCOMING_CAPACITY: usize = 64;
    /// 服务端 writer 前的有序响应/事件容量。
    pub const SERVER_OUTGOING_CAPACITY: usize = 64;
    /// Windows 扩展长度路径允许的 UTF-16 code unit 数量。
    pub const PROCESS_PATH_BUFFER_LEN: usize = 32_768;
}
