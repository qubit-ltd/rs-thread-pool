// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use qubit_executor::service::SubmissionError;
use thiserror::Error;

/// Error returned by the low-level custom-job submission API.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PoolJobSubmissionError {
    /// The pool rejected the job before acceptance.
    #[error(transparent)]
    Rejected(#[from] SubmissionError),
    /// The custom acceptance callback panicked before the job was published.
    #[error("pool job acceptance callback panicked")]
    AcceptancePanicked,
}

impl PoolJobSubmissionError {
    /// Converts this error to the common executor error for standard jobs.
    pub(crate) fn into_submission_error(self) -> SubmissionError {
        match self {
            Self::Rejected(error) => error,
            Self::AcceptancePanicked => {
                unreachable!("standard pool jobs cannot panic during acceptance")
            }
        }
    }
}
