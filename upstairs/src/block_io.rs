// Copyright 2021 Oxide Computer Company

use super::*;

use std::fs::{File, OpenOptions};
use std::io::SeekFrom;

// Implement BlockIO for a file

pub struct FileBlockIO {
    uuid: Uuid,
    block_size: u64,
    total_size: u64,
    file: Mutex<File>,
}

impl FileBlockIO {
    pub fn new(block_size: u64, path: String) -> Result<Self> {
        match OpenOptions::new().read(true).write(true).open(&path) {
            Err(e) => {
                bail!("Error: e {} No extent file found for {:?}", e, path);
            }
            Ok(f) => {
                let total_size = f.metadata()?.len();

                Ok(Self {
                    uuid: Uuid::new_v4(),
                    block_size,
                    total_size: total_size as u64,
                    file: Mutex::new(f),
                })
            }
        }
    }
}

impl BlockIO for FileBlockIO {
    fn activate(&self, _gen: u64) -> Result<(), CrucibleError> {
        Ok(())
    }

    fn query_is_active(&self) -> Result<bool, CrucibleError> {
        Ok(true)
    }

    fn total_size(&self) -> Result<u64, CrucibleError> {
        Ok(self.total_size)
    }

    fn get_block_size(&self) -> Result<u64, CrucibleError> {
        Ok(self.block_size)
    }

    fn get_uuid(&self) -> Result<Uuid, CrucibleError> {
        Ok(self.uuid)
    }

    fn read(
        &self,
        offset: Block,
        data: Buffer,
    ) -> Result<BlockReqWaiter, CrucibleError> {
        let mut data_vec = data.as_vec();
        let mut owned_vec = data.owned_vec();

        let start = offset.value * self.block_size;

        let mut file = self.file.lock().unwrap();
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut data_vec[..])?;

        for i in 0..data_vec.len() {
            owned_vec[i] = true;
        }

        BlockReqWaiter::immediate()
    }

    fn write(
        &self,
        offset: Block,
        data: Bytes,
    ) -> Result<BlockReqWaiter, CrucibleError> {
        let start = offset.value * self.block_size;

        let mut file = self.file.lock().unwrap();
        file.seek(SeekFrom::Start(start))?;
        file.write_all(&data[..])?;

        BlockReqWaiter::immediate()
    }

    fn flush(&self) -> Result<BlockReqWaiter, CrucibleError> {
        let mut file = self.file.lock().unwrap();
        file.flush()?;
        BlockReqWaiter::immediate()
    }

    fn show_work(&self) -> Result<WQCounts, CrucibleError> {
        Ok(WQCounts {
            up_count: 0,
            ds_count: 0,
        })
    }
}

// Implement BlockIO over an HTTP(S) url
use reqwest::blocking::Client;
use reqwest::header::{CONTENT_LENGTH, RANGE};
use std::str::FromStr;

pub struct ReqwestBlockIO {
    uuid: Uuid,
    block_size: u64,
    total_size: u64,
    client: Client,
    url: String,
}

impl ReqwestBlockIO {
    pub fn new(block_size: u64, url: String) -> Result<Self, CrucibleError> {
        let client = Client::new();

        let response = client
            .head(&url)
            .send()
            .map_err(|e| CrucibleError::GenericError(e.to_string()))?;
        let content_length = response
            .headers()
            .get(CONTENT_LENGTH)
            .ok_or("no content length!")
            .map_err(|e| CrucibleError::GenericError(e.to_string()))?;
        let total_size = u64::from_str(
            content_length
                .to_str()
                .map_err(|e| CrucibleError::GenericError(e.to_string()))?,
        )
        .map_err(|e| CrucibleError::GenericError(e.to_string()))?;

        Ok(Self {
            uuid: Uuid::new_v4(),
            block_size,
            total_size: total_size as u64,
            client,
            url,
        })
    }
}

impl BlockIO for ReqwestBlockIO {
    fn activate(&self, _gen: u64) -> Result<(), CrucibleError> {
        Ok(())
    }

    fn query_is_active(&self) -> Result<bool, CrucibleError> {
        Ok(true)
    }

    fn total_size(&self) -> Result<u64, CrucibleError> {
        Ok(self.total_size)
    }

    fn get_block_size(&self) -> Result<u64, CrucibleError> {
        Ok(self.block_size)
    }

    fn get_uuid(&self) -> Result<Uuid, CrucibleError> {
        Ok(self.uuid)
    }

    fn read(
        &self,
        offset: Block,
        data: Buffer,
    ) -> Result<BlockReqWaiter, CrucibleError> {
        let mut data_vec = data.as_vec();
        let mut owned_vec = data.owned_vec();

        let start = offset.value * self.block_size;

        let response = self
            .client
            .get(&self.url)
            .header(
                RANGE,
                format!(
                    "bytes={}-{}",
                    start,
                    start + data_vec.len() as u64 - 1
                ),
            )
            .send()
            .map_err(|e| CrucibleError::GenericError(e.to_string()))?;

        let content_length = response
            .headers()
            .get(CONTENT_LENGTH)
            .ok_or("no content length!")
            .map_err(|e| CrucibleError::GenericError(e.to_string()))?;
        let total_size = u64::from_str(
            content_length
                .to_str()
                .map_err(|e| CrucibleError::GenericError(e.to_string()))?,
        )
        .map_err(|e| CrucibleError::GenericError(e.to_string()))?;

        assert_eq!(total_size, data_vec.len() as u64);

        let bytes = response
            .bytes()
            .map_err(|e| CrucibleError::GenericError(e.to_string()))?;

        for i in 0..data_vec.len() {
            data_vec[i] = bytes[i];
            owned_vec[i] = true;
        }

        BlockReqWaiter::immediate()
    }

    fn write(
        &self,
        _offset: Block,
        _data: Bytes,
    ) -> Result<BlockReqWaiter, CrucibleError> {
        unimplemented!();
    }

    fn flush(&self) -> Result<BlockReqWaiter, CrucibleError> {
        BlockReqWaiter::immediate()
    }

    fn show_work(&self) -> Result<WQCounts, CrucibleError> {
        Ok(WQCounts {
            up_count: 0,
            ds_count: 0,
        })
    }
}