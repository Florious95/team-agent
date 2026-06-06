//! 用户面错误。
//!
//! 陷阱 #6(02-model 卡):Python `errors.py` 用自定义 `RuntimeError` **遮蔽**内建
//! `RuntimeError`。Rust 无此坑,但**必须保留两类区分**:`Validation`(spec/envelope/输入)
//! vs `Runtime`(运行期/team 解析),别合并成一个。§12:lib 层用 `thiserror`。

use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ModelError {
    /// spec / result-envelope / 输入校验失败 —— 对应 Python `ValidationError`。
    #[error("validation error: {0}")]
    Validation(String),
    /// 运行期 / team 解析 / 身份派生失败 —— 对应 Python(自定义)`RuntimeError`。
    #[error("runtime error: {0}")]
    Runtime(String),
}
