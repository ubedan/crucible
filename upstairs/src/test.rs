// Copyright 2023 Oxide Computer Company

use crate::downstairs::Downstairs;
use crate::*;

pub(crate) fn generic_read_request() -> (ReadRequest, ImpactedBlocks) {
    let request = ReadRequest {
        eid: 0,
        offset: Block::new_512(7),
    };
    let iblocks = ImpactedBlocks::new(
        ImpactedAddr {
            extent_id: 0,
            block: 7,
        },
        ImpactedAddr {
            extent_id: 0,
            block: 7,
        },
    );
    (request, iblocks)
}

pub(crate) fn generic_write_request(
) -> (crucible_protocol::Write, ImpactedBlocks) {
    let request = crucible_protocol::Write {
        eid: 0,
        offset: Block::new_512(7),
        data: Bytes::from(vec![1]),
        block_context: BlockContext {
            encryption_context: None,
            hash: 0,
        },
    };
    let iblocks = ImpactedBlocks::new(
        ImpactedAddr {
            extent_id: 0,
            block: 7,
        },
        ImpactedAddr {
            extent_id: 0,
            block: 7,
        },
    );
    (request, iblocks)
}

pub(crate) fn create_generic_read_eob(
    ds: &mut Downstairs,
    ds_id: JobId,
) -> (ReadRequest, DownstairsIO) {
    let (request, iblocks) = generic_read_request();
    let op = ds.create_read_eob(ds_id, iblocks, 10, vec![request.clone()]);

    (request, op)
}

/*
 * Beware, if you change these defaults, then you will have to change
 * all the hard coded tests below that use make_upstairs().
 */
pub(crate) fn make_upstairs() -> crate::upstairs::Upstairs {
    let mut def = RegionDefinition::default();
    def.set_block_size(512);
    def.set_extent_size(Block::new_512(100));
    def.set_extent_count(10);

    let opts = CrucibleOpts {
        target: vec![],
        lossy: false,
        key: None,
        ..Default::default()
    };

    crate::upstairs::Upstairs::new(
        &opts,
        0,
        Some(def),
        Arc::new(Guest::new()),
        None,
        crucible_common::build_logger(),
    )
}

#[cfg(feature = "NOPE")]
pub(crate) mod up_test {
    use super::*;
    use crate::{
        client::{
            validate_encrypted_read_response,
            validate_unencrypted_read_response,
        },
        downstairs::Downstairs,
        upstairs::{Upstairs, UpstairsState},
    };
    use rand::prelude::*;

    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use base64::{engine, Engine};
    use itertools::Itertools;
    use pseudo_file::IOSpan;
    use ringbuffer::RingBuffer;
    use tokio::sync::watch;

    // Create a simple logger
    pub fn csl() -> Logger {
        build_logger()
    }

    fn extent_tuple(eid: u64, offset: u64) -> (u64, Block) {
        (eid, Block::new_512(offset))
    }

    #[test]
    fn test_iospan() {
        let span = IOSpan::new(512, 1024, 512);
        assert!(span.is_block_regular());
        assert_eq!(span.affected_block_count(), 2);

        let span = IOSpan::new(513, 1024, 512);
        assert!(!span.is_block_regular());
        assert_eq!(span.affected_block_count(), 3);

        let span = IOSpan::new(512, 500, 512);
        assert!(!span.is_block_regular());
        assert_eq!(span.affected_block_count(), 1);

        let span = IOSpan::new(512, 512, 4096);
        assert!(!span.is_block_regular());
        assert_eq!(span.affected_block_count(), 1);

        let span = IOSpan::new(500, 4096 * 10, 4096);
        assert!(!span.is_block_regular());
        assert_eq!(span.affected_block_count(), 10 + 1);

        let span = IOSpan::new(500, 4096 * 3 + (4096 - 500 + 1), 4096);
        assert!(!span.is_block_regular());
        assert_eq!(span.affected_block_count(), 3 + 2);

        // Some from hammer
        let span = IOSpan::new(137690, 1340, 512);
        assert!(!span.is_block_regular());
        assert_eq!(span.affected_block_count(), 4);
        assert_eq!(span.affected_block_numbers(), &vec![268, 269, 270, 271]);
    }

    #[tokio::test]
    async fn test_iospan_buffer_read_write() {
        let span = IOSpan::new(500, 64, 512);
        assert_eq!(span.affected_block_count(), 2);
        assert_eq!(span.affected_block_numbers(), &vec![0, 1]);

        span.write_from_buffer_into_blocks(&Bytes::from(vec![1; 64]))
            .await;

        for i in 0..500 {
            assert_eq!(span.buffer().as_vec().await[i], 0);
        }
        for i in 500..512 {
            assert_eq!(span.buffer().as_vec().await[i], 1);
        }
        for i in 512..(512 + 64 - 12) {
            assert_eq!(span.buffer().as_vec().await[i], 1);
        }
        for i in (512 + 64 - 12)..1024 {
            assert_eq!(span.buffer().as_vec().await[i], 0);
        }

        let data = Buffer::new(64);
        span.read_from_blocks_into_buffer(&mut data.as_vec().await[..])
            .await;

        for i in 0..64 {
            assert_eq!(data.as_vec().await[i], 1);
        }
    }

    /*
     * Terrible wrapper, but it allows us to call extent_from_offset()
     * just like the program does.
     */
    async fn up_efo(
        up: &Upstairs,
        offset: Block,
        num_blocks: u64,
    ) -> Vec<(u64, Block)> {
        let ddef = up.get_region_definition();
        let num_blocks = Block::new_with_ddef(num_blocks, &ddef);
        extent_from_offset(&ddef, offset, num_blocks)
            .blocks(&ddef)
            .collect()
    }

    #[tokio::test]
    async fn off_to_extent_one_block() {
        let up = make_upstairs();

        for i in 0..100 {
            let exv = vec![extent_tuple(0, i)];
            assert_eq!(up_efo(&up, Block::new_512(i), 1).await, exv);
        }

        for i in 0..100 {
            let exv = vec![extent_tuple(1, i)];
            assert_eq!(up_efo(&up, Block::new_512(100 + i), 1).await, exv);
        }

        let exv = vec![extent_tuple(2, 0)];
        assert_eq!(up_efo(&up, Block::new_512(200), 1).await, exv);

        let exv = vec![extent_tuple(9, 99)];
        assert_eq!(up_efo(&up, Block::new_512(999), 1).await, exv);
    }

    #[tokio::test]
    async fn off_to_extent_two_blocks() {
        let up = make_upstairs();

        for i in 0..99 {
            let exv = vec![extent_tuple(0, i), extent_tuple(0, i + 1)];
            assert_eq!(up_efo(&up, Block::new_512(i), 2).await, exv);
        }

        let exv = vec![extent_tuple(0, 99), extent_tuple(1, 0)];
        assert_eq!(up_efo(&up, Block::new_512(99), 2).await, exv);

        for i in 0..99 {
            let exv = vec![extent_tuple(1, i)];
            assert_eq!(up_efo(&up, Block::new_512(100 + i), 1).await, exv);
        }

        let exv = vec![extent_tuple(1, 99), extent_tuple(2, 0)];
        assert_eq!(up_efo(&up, Block::new_512(199), 2).await, exv);

        let exv = vec![extent_tuple(2, 0), extent_tuple(2, 1)];
        assert_eq!(up_efo(&up, Block::new_512(200), 2).await, exv);

        let exv = vec![extent_tuple(9, 98), extent_tuple(9, 99)];
        assert_eq!(up_efo(&up, Block::new_512(998), 2).await, exv);
    }

    #[tokio::test]
    async fn off_to_extent_bridge() {
        /*
         * Testing when our buffer crosses extents.
         */
        let up = make_upstairs();

        /*
         * 1024 buffer
         */
        assert_eq!(
            up_efo(&up, Block::new_512(99), 2).await,
            vec![extent_tuple(0, 99), extent_tuple(1, 0)],
        );
        assert_eq!(
            up_efo(&up, Block::new_512(98), 4).await,
            vec![
                extent_tuple(0, 98),
                extent_tuple(0, 99),
                extent_tuple(1, 0),
                extent_tuple(1, 1),
            ],
        );

        /*
         * Largest buffer at different offsets
         */
        for offset in 0..100 {
            let expected: Vec<(u64, Block)> = (0..100)
                .map(|i| extent_tuple((offset + i) / 100, (offset + i) % 100))
                .collect();
            assert_eq!(
                up_efo(&up, Block::new_512(offset), 100).await,
                expected
            );
        }
    }

    /*
     * Testing various invalid inputs
     */
    #[tokio::test]
    async fn off_to_extent_length_zero() {
        let up = make_upstairs();
        assert_eq!(up_efo(&up, Block::new_512(0), 0).await, vec![]);
    }

    #[tokio::test]
    async fn off_to_extent_length_almost_too_big() {
        let up = make_upstairs();
        up_efo(&up, Block::new_512(0), 1000).await;
    }

    #[tokio::test]
    #[should_panic]
    async fn off_to_extent_length_too_big() {
        let up = make_upstairs();
        up_efo(&up, Block::new_512(0), 1001).await;
    }

    #[tokio::test]
    async fn off_to_extent_length_and_offset_almost_too_big() {
        let up = make_upstairs();
        up_efo(&up, Block::new_512(900), 100).await;
    }

    #[tokio::test]
    #[should_panic]
    async fn off_to_extent_length_and_offset_too_big() {
        let up = make_upstairs();
        up_efo(&up, Block::new_512(1000), 1).await;
    }

    #[tokio::test]
    #[should_panic]
    async fn not_right_block_size() {
        let up = make_upstairs();
        up_efo(&up, Block::new_4096(900), 1).await;
    }

    // key material made with `openssl rand -base64 32`
    #[test]
    pub fn test_upstairs_encryption_context_ok() -> Result<()> {
        use rand::{thread_rng, Rng};

        let key_bytes = engine::general_purpose::STANDARD
            .decode("ClENKTXD2bCyXSHnKXY7GGnk+NvQKbwpatjWP2fJzk0=")
            .unwrap();
        let context = EncryptionContext::new(key_bytes, 512);

        let mut block = [0u8; 512];
        thread_rng().fill(&mut block[..]);

        let orig_block = block;

        let (nonce, tag, _) = context.encrypt_in_place(&mut block[..])?;
        assert_ne!(block, orig_block);

        context.decrypt_in_place(&mut block[..], &nonce, &tag)?;
        assert_eq!(block, orig_block);

        Ok(())
    }

    #[test]
    pub fn test_upstairs_encryption_context_wrong_nonce() -> Result<()> {
        use rand::{thread_rng, Rng};

        let key_bytes = engine::general_purpose::STANDARD
            .decode("EVrH+ABhMP0MLfxynCalDq1vWCCWCWFfsSsJoJeDCx8=")
            .unwrap();
        let context = EncryptionContext::new(key_bytes, 512);

        let mut block = [0u8; 512];
        thread_rng().fill(&mut block[..]);

        let orig_block = block;

        let (_, tag, _) = context.encrypt_in_place(&mut block[..])?;
        assert_ne!(block, orig_block);

        let nonce = context.get_random_nonce();

        let block_before_failing_decrypt_in_place = block;

        let result = context.decrypt_in_place(&mut block[..], &nonce, &tag);
        assert!(result.is_err());

        /*
         * Make sure encryption context does not overwrite data if it's given
         * a bad nonce - we rely on this and do not make a copy when
         * attempting to decrypt with multiple encryption contexts.
         */
        assert_eq!(block_before_failing_decrypt_in_place, block);

        Ok(())
    }

    #[test]
    pub fn test_upstairs_encryption_context_wrong_tag() -> Result<()> {
        use rand::{thread_rng, Rng};

        let key_bytes = engine::general_purpose::STANDARD
            .decode("EVrH+ABhMP0MLfxynCalDq1vWCCWCWFfsSsJoJeDCx8=")
            .unwrap();
        let context = EncryptionContext::new(key_bytes, 512);

        let mut block = [0u8; 512];
        thread_rng().fill(&mut block[..]);

        let orig_block = block;

        let (nonce, mut tag, _) = context.encrypt_in_place(&mut block[..])?;
        assert_ne!(block, orig_block);

        tag[2] = tag[2].wrapping_add(1);

        let block_before_failing_decrypt_in_place = block;

        let result = context.decrypt_in_place(&mut block[..], &nonce, &tag);
        assert!(result.is_err());

        /*
         * Make sure encryption context does not overwrite data if it's given
         * a bad tag - we rely on this and do not make a copy when attempting
         * to decrypt with multiple encryption contexts.
         */
        assert_eq!(block_before_failing_decrypt_in_place, block);

        Ok(())
    }

    // Validate that an encrypted read response with one context can be
    // decrypted
    #[test]
    pub fn test_upstairs_validate_encrypted_read_response() -> Result<()> {
        // Set up the encryption context
        use rand::{thread_rng, Rng};
        let mut key = vec![0u8; 32];
        thread_rng().fill(&mut key[..]);
        let context = EncryptionContext::new(key.clone(), 512);

        // Encrypt some random data
        let mut data = BytesMut::with_capacity(512);
        data.resize(512, 0u8);
        thread_rng().fill(&mut data[..]);

        let original_data = data.clone();

        let (nonce, tag, _) = context.encrypt_in_place(&mut data[..])?;

        assert_ne!(original_data, data);

        let read_response_hash = integrity_hash(&[&nonce, &tag, &data[..]]);

        // Create the read response
        let mut read_response = ReadResponse {
            eid: 0,
            offset: Block::new_512(0),
            data,
            block_contexts: vec![BlockContext {
                hash: read_response_hash,
                encryption_context: Some(
                    crucible_protocol::EncryptionContext {
                        nonce: nonce.into(),
                        tag: tag.into(),
                    },
                ),
            }],
        };

        // Validate it
        let successful_hash = validate_encrypted_read_response(
            &mut read_response,
            &Arc::new(context),
            &csl(),
        )?;

        assert_eq!(successful_hash, Some(read_response_hash));

        // `validate_encrypted_read_response` will mutate the read
        // response's data value, make sure it decrypted

        assert_eq!(original_data, read_response.data);

        Ok(())
    }

    // Validate that an encrypted read response with multiple contexts can be
    // decrypted (skipping ones that don't match)
    #[test]
    pub fn test_upstairs_validate_encrypted_read_response_multiple_contexts(
    ) -> Result<()> {
        // Set up the encryption context
        use rand::{thread_rng, Rng};
        let mut key = vec![0u8; 32];
        thread_rng().fill(&mut key[..]);
        let context = EncryptionContext::new(key.clone(), 512);

        // Encrypt some random data
        let mut data = BytesMut::with_capacity(512);
        data.resize(512, 0u8);
        thread_rng().fill(&mut data[..]);

        let original_data = data.clone();

        let (nonce, tag, _) = context.encrypt_in_place(&mut data[..])?;

        assert_ne!(original_data, data);

        let read_response_hash = integrity_hash(&[&nonce, &tag, &data[..]]);

        // Create the read response
        let mut read_response = ReadResponse {
            eid: 0,
            offset: Block::new_512(0),
            data,
            block_contexts: vec![
                // The first context here doesn't match
                BlockContext {
                    hash: thread_rng().gen(),
                    encryption_context: Some(
                        crucible_protocol::EncryptionContext {
                            nonce: thread_rng().gen::<[u8; 12]>(),
                            tag: thread_rng().gen::<[u8; 16]>(),
                        },
                    ),
                },
                // This context matches
                BlockContext {
                    hash: read_response_hash,
                    encryption_context: Some(
                        crucible_protocol::EncryptionContext {
                            nonce: nonce.into(),
                            tag: tag.into(),
                        },
                    ),
                },
                // The last context does not
                BlockContext {
                    hash: thread_rng().gen(),
                    encryption_context: Some(
                        crucible_protocol::EncryptionContext {
                            nonce: thread_rng().gen::<[u8; 12]>(),
                            tag: thread_rng().gen::<[u8; 16]>(),
                        },
                    ),
                },
            ],
        };

        // Validate it
        let successful_hash = validate_encrypted_read_response(
            &mut read_response,
            &Arc::new(context),
            &csl(),
        )?;

        assert_eq!(successful_hash, Some(read_response_hash));

        // `validate_encrypted_read_response` will mutate the read
        // response's data value, make sure it decrypted

        assert_eq!(original_data, read_response.data);

        Ok(())
    }

    // TODO if such a set of nonces and tags can be found:
    //
    //   let hash1 = integrity_hash(
    //      &[&ctx1.nonce[..], &ctx1.tag[..], &response.data[..]]
    //   );
    //   let hash2 = integrity_hash(
    //      &[&ctx2.nonce[..], &ctx2.tag[..], &response.data[..]]
    //   );
    //
    //   hash1 == hash2
    //
    // then write a test which validates that an encrypted read response with
    // multiple contexts that match the integrity hash (where only one is
    // correct) can be decrypted.

    // Validate that reading a blank block works
    #[test]
    pub fn test_upstairs_validate_encrypted_read_response_blank_block(
    ) -> Result<()> {
        // Set up the encryption context
        use rand::{thread_rng, Rng};
        let mut key = vec![0u8; 32];
        thread_rng().fill(&mut key[..]);
        let context = EncryptionContext::new(key.clone(), 512);

        let mut data = BytesMut::with_capacity(512);
        data.resize(512, 0u8);

        // Create the read response
        let mut read_response = ReadResponse {
            eid: 0,
            offset: Block::new_512(0),
            data: data.clone(),
            block_contexts: vec![],
        };

        // Validate it
        let successful_hash = validate_encrypted_read_response(
            &mut read_response,
            &Arc::new(context),
            &csl(),
        )?;

        // The above function will return None for a blank block
        assert_eq!(successful_hash, None);
        assert_eq!(data, vec![0u8; 512]);

        Ok(())
    }

    #[test]
    pub fn test_upstairs_validate_unencrypted_read_response() -> Result<()> {
        use rand::{thread_rng, Rng};

        let mut data = BytesMut::with_capacity(512);
        data.resize(512, 0u8);
        thread_rng().fill(&mut data[..]);

        let read_response_hash = integrity_hash(&[&data[..]]);
        let original_data = data.clone();

        // Create the read response
        let mut read_response = ReadResponse {
            eid: 0,
            offset: Block::new_512(0),
            data,
            block_contexts: vec![BlockContext {
                hash: read_response_hash,
                encryption_context: None,
            }],
        };

        // Validate it
        let successful_hash =
            validate_unencrypted_read_response(&mut read_response, &csl())?;

        assert_eq!(successful_hash, Some(read_response_hash));
        assert_eq!(read_response.data, original_data);

        Ok(())
    }

    #[test]
    pub fn test_upstairs_validate_unencrypted_read_response_blank_block(
    ) -> Result<()> {
        let mut data = BytesMut::with_capacity(512);
        data.resize(512, 0u8);

        let original_data = data.clone();

        // Create the read response
        let mut read_response = ReadResponse {
            eid: 0,
            offset: Block::new_512(0),
            data,
            block_contexts: vec![],
        };

        // Validate it
        let successful_hash =
            validate_unencrypted_read_response(&mut read_response, &csl())?;

        assert_eq!(successful_hash, None);
        assert_eq!(read_response.data, original_data);

        Ok(())
    }

    #[test]
    pub fn test_upstairs_validate_unencrypted_read_response_multiple_contexts(
    ) -> Result<()> {
        use rand::{thread_rng, Rng};

        let mut data = BytesMut::with_capacity(512);
        data.resize(512, 0u8);
        thread_rng().fill(&mut data[..]);

        let read_response_hash = integrity_hash(&[&data[..]]);
        let original_data = data.clone();

        // Create the read response
        let mut read_response = ReadResponse {
            eid: 0,
            offset: Block::new_512(0),
            data,
            block_contexts: vec![
                // The context here doesn't match
                BlockContext {
                    hash: thread_rng().gen(),
                    encryption_context: None,
                },
                // The context here doesn't match
                BlockContext {
                    hash: thread_rng().gen(),
                    encryption_context: None,
                },
                // Correct one
                BlockContext {
                    hash: read_response_hash,
                    encryption_context: None,
                },
                // The context here doesn't match
                BlockContext {
                    hash: thread_rng().gen(),
                    encryption_context: None,
                },
            ],
        };

        // Validate it
        let successful_hash =
            validate_unencrypted_read_response(&mut read_response, &csl())?;

        assert_eq!(successful_hash, Some(read_response_hash));
        assert_eq!(read_response.data, original_data);

        Ok(())
    }

    // Validate that an unencrypted read response with multiple contexts that
    // match the integrity hash works. This can happen if the Upstairs
    // repeatedly writes the same block data.
    #[test]
    pub fn test_upstairs_validate_unencrypted_read_response_multiple_hashes(
    ) -> Result<()> {
        use rand::{thread_rng, Rng};

        let mut data = BytesMut::with_capacity(512);
        data.resize(512, 0u8);
        thread_rng().fill(&mut data[..]);

        let read_response_hash = integrity_hash(&[&data[..]]);
        let original_data = data.clone();

        // Create the read response
        let mut read_response = ReadResponse {
            eid: 0,
            offset: Block::new_512(0),
            data,
            block_contexts: vec![
                // Correct one
                BlockContext {
                    hash: read_response_hash,
                    encryption_context: None,
                },
                // Correct one
                BlockContext {
                    hash: read_response_hash,
                    encryption_context: None,
                },
            ],
        };

        // Validate it
        let successful_hash =
            validate_unencrypted_read_response(&mut read_response, &csl())?;

        assert_eq!(successful_hash, Some(read_response_hash));
        assert_eq!(read_response.data, original_data);

        Ok(())
    }

    #[tokio::test]
    async fn write_unwritten_single_skip() {
        // Verfy that a write_unwritten with one skipped job will still
        // result in success sent back to the guest.
        w_io_single_skip(true).await;
    }
    #[tokio::test]
    async fn write_single_skip() {
        // Verfy that a write with one skipped job will still result in
        // success being sent back to the guest.
        w_io_single_skip(false).await;
    }

    async fn w_io_single_skip(is_write_unwritten: bool) {
        // up_ds_listen test, a single downstairs skip won't prevent us
        // from acking back OK to the guest.
        let up = Upstairs::test_default(None);
        let (ds_done_tx, _ds_done_rx) = mpsc::channel(500);
        up.force_active().unwrap();
        for cid in ClientId::iter() {
            up.force_ds_state(cid, DsState::WaitActive);
            up.force_ds_state(cid, DsState::WaitQuorum);
            up.force_ds_state(cid, DsState::Active);
        }
        up.force_ds_state(ClientId::new(1), DsState::Faulted);

        let mut gw = up.guest.guest_work.lock().await;
        let ds = &mut up.downstairs;

        let gw_id = 19;
        let next_id = JobId(1010);

        // Create a write, enqueue it on both the downstairs
        // and the guest work queues.
        let (request, iblocks) = generic_write_request();
        let next_id =
            ds.submit_write(gw_id, iblocks, vec![request], is_write_unwritten);
        let new_gtos = GtoS::new(next_id, None, None);
        {
            gw.active.insert(gw_id, new_gtos);
        }
        drop(gw);

        assert!(ds.in_progress(next_id, ClientId::new(0)).is_some());
        assert!(ds.in_progress(next_id, ClientId::new(2)).is_some());

        let response = Ok(vec![]);
        ds.process_ds_completion(
            next_id,
            ClientId::new(0),
            response.clone(),
            &UpstairsState::Active,
            None,
        )
        .unwrap();

        ds.process_ds_completion(
            next_id,
            ClientId::new(2),
            response,
            &UpstairsState::Active,
            None,
        )
        .unwrap();

        let ack_list = ds.ackable_work();
        assert_eq!(ack_list.len(), 1);

        // Simulation of what happens in up_ds_listen
        for ds_id_done in ack_list.iter() {
            assert_eq!(*ds_id_done, next_id);

            let done = ds.ds_active.get(ds_id_done).unwrap();
            assert_eq!(done.ack_status, AckStatus::AckReady);

            assert_eq!(done.guest_id, gw_id);

            ds.ack(*ds_id_done);

            assert!(ds.result(*ds_id_done).is_ok());
        }
    }

    #[tokio::test]
    async fn write_unwritten_double_skip() {
        // Verify that write IO errors are counted.
        w_io_double_skip(true).await;
    }
    #[tokio::test]
    async fn write_double_skip() {
        // Verify that write IO errors are counted.
        w_io_double_skip(false).await;
    }

    async fn w_io_double_skip(is_write_unwritten: bool) {
        // up_ds_listen test, a double skip on a write or write_unwritten
        // will result in an error back to the guest.
        let up = Upstairs::test_default(None);
        let (ds_done_tx, _ds_done_rx) = mpsc::channel(500);
        up.force_active().unwrap();
        for cid in ClientId::iter() {
            up.force_ds_state(cid, DsState::WaitActive);
            up.force_ds_state(cid, DsState::WaitQuorum);
            up.force_ds_state(cid, DsState::Active);
        }
        up.force_ds_state(ClientId::new(1), DsState::Faulted);
        up.force_ds_state(ClientId::new(2), DsState::Faulted);

        let mut gw = up.guest.guest_work.lock().await;
        let ds = &mut up.downstairs;

        let gw_id = 19;
        let next_id = JobId(1010);

        // Create a write, enqueue it on both the downstairs
        // and the guest work queues.
        let (request, iblocks) = generic_write_request();
        let next_id =
            ds.submit_write(gw_id, iblocks, vec![request], is_write_unwritten);

        let new_gtos = GtoS::new(next_id, None, None);
        {
            gw.active.insert(gw_id, new_gtos);
        }
        drop(gw);

        assert!(ds.in_progress(next_id, ClientId::new(0)).is_some());

        ds.process_ds_completion(
            next_id,
            ClientId::new(0),
            Ok(vec![]),
            &UpstairsState::Active,
            None,
        )
        .unwrap();

        let ack_list = ds.ackable_work();
        assert_eq!(ack_list.len(), 1);

        // Simulation of what happens in up_ds_listen
        for ds_id_done in ack_list.iter() {
            assert_eq!(*ds_id_done, next_id);

            let done = ds.ds_active.get(ds_id_done).unwrap();
            assert_eq!(done.ack_status, AckStatus::AckReady);

            assert_eq!(done.guest_id, gw_id);
            ds.ack(*ds_id_done);

            assert!(ds.result(*ds_id_done).is_err());
        }
    }

    #[tokio::test]
    async fn write_unwritten_fail_and_skip() {
        // Verify that an write_unwritten error + a skip results in error
        // back to the guest
        w_io_fail_and_skip(true).await;
    }
    #[tokio::test]
    async fn write_fail_and_skip() {
        // Verify that a write error + a skip results in error back to the
        // guest
        w_io_fail_and_skip(false).await;
    }

    async fn w_io_fail_and_skip(is_write_unwritten: bool) {
        // up_ds_listen test, a fail plus a skip on a write or write_unwritten
        // will result in an error back to the guest.
        let up = Upstairs::test_default(None);
        let (ds_done_tx, _ds_done_rx) = mpsc::channel(500);
        up.force_active().unwrap();
        for cid in ClientId::iter() {
            up.force_ds_state(cid, DsState::WaitActive);
            up.force_ds_state(cid, DsState::WaitQuorum);
            up.force_ds_state(cid, DsState::Active);
        }
        up.force_ds_state(ClientId::new(2), DsState::Faulted);

        let mut gw = up.guest.guest_work.lock().await;
        let ds = &mut up.downstairs;

        let gw_id = 19;
        let next_id = JobId(1010);

        // Create a write, enqueue it on both the downstairs
        // and the guest work queues.
        let (request, iblocks) = generic_write_request();
        let next_id =
            ds.submit_write(gw_id, iblocks, vec![request], is_write_unwritten);

        let new_gtos = GtoS::new(next_id, None, None);
        {
            gw.active.insert(gw_id, new_gtos);
        }
        drop(gw);

        assert!(ds.in_progress(next_id, ClientId::new(0)).is_some());
        assert!(ds.in_progress(next_id, ClientId::new(1)).is_some());

        // DS 0, the good IO.
        ds.process_ds_completion(
            next_id,
            ClientId::new(0),
            Ok(vec![]),
            &UpstairsState::Active,
            None,
        )
        .unwrap();

        // DS 1, return error.
        // On a write_unwritten, This will return true because we have now
        // completed all three IOs (skipped, ok, and error here).
        let res = ds
            .process_ds_completion(
                next_id,
                ClientId::new(1),
                Err(CrucibleError::GenericError("bad".to_string())),
                &UpstairsState::Active,
                None,
            )
            .unwrap();

        assert_eq!(res, is_write_unwritten);

        let ack_list = ds.ackable_work();
        assert_eq!(ack_list.len(), 1);

        // Simulation of what happens in up_ds_listen
        for ds_id_done in ack_list.iter() {
            assert_eq!(*ds_id_done, next_id);

            let done = ds.ds_active.get(ds_id_done).unwrap();
            assert_eq!(done.ack_status, AckStatus::AckReady);

            assert_eq!(done.guest_id, gw_id);

            ds.ack(*ds_id_done);

            assert!(ds.result(*ds_id_done).is_err());
        }
        info!(up.log, "All done");
    }

    #[tokio::test]
    async fn flush_io_single_skip() {
        // up_ds_listen test, a single downstairs skip won't prevent us
        // from acking back OK for a flush to the guest.
        let up = Upstairs::test_default(None);
        let (ds_done_tx, _ds_done_rx) = mpsc::channel(500);
        up.force_active().unwrap();
        for cid in ClientId::iter() {
            up.force_ds_state(cid, DsState::WaitActive);
            up.force_ds_state(cid, DsState::WaitQuorum);
            up.force_ds_state(cid, DsState::Active);
        }
        up.force_ds_state(ClientId::new(1), DsState::Faulted);

        let mut gw = up.guest.guest_work.lock().await;
        let ds = &mut up.downstairs;

        let gw_id = 19;
        let next_id = JobId(1010);

        // Create a flush, enqueue it on both the downstairs
        // and the guest work queues.
        let dep = ds.ds_active.deps_for_flush(next_id);
        let op = create_flush(
            next_id, dep, 22, // Flush number
            gw_id, 11, // Gen number
            None, None,
        );

        let new_gtos = GtoS::new(next_id, None, None);
        {
            gw.active.insert(gw_id, new_gtos);
        }
        drop(gw);

        ds.enqueue(op, ds_done_tx.clone()).await;

        assert!(ds.in_progress(next_id, ClientId::new(0)).is_some());
        assert!(ds.in_progress(next_id, ClientId::new(2)).is_some());

        let response = Ok(vec![]);
        ds.process_ds_completion(
            next_id,
            ClientId::new(0),
            response.clone(),
            &UpstairsState::Active,
            None,
        )
        .unwrap();

        ds.process_ds_completion(
            next_id,
            ClientId::new(2),
            response,
            &UpstairsState::Active,
            None,
        )
        .unwrap();

        let ack_list = ds.ackable_work();
        assert_eq!(ack_list.len(), 1);

        // Simulation of what happens in up_ds_listen
        for ds_id_done in ack_list.iter() {
            assert_eq!(*ds_id_done, next_id);

            let done = ds.ds_active.get(ds_id_done).unwrap();
            assert_eq!(done.ack_status, AckStatus::AckReady);

            assert_eq!(done.guest_id, gw_id);
            ds.ack(*ds_id_done);

            assert!(ds.result(*ds_id_done).is_ok());
        }
    }

    #[tokio::test]
    async fn flush_io_double_skip() {
        // up_ds_listen test, a double skip on a flush will result in an error
        // back to the guest.
        let up = Upstairs::test_default(None);
        let (ds_done_tx, _ds_done_rx) = mpsc::channel(500);
        up.force_active().unwrap();
        for cid in ClientId::iter() {
            up.force_ds_state(cid, DsState::WaitActive);
            up.force_ds_state(cid, DsState::WaitQuorum);
            up.force_ds_state(cid, DsState::Active);
        }
        up.force_ds_state(ClientId::new(1), DsState::Faulted);
        up.force_ds_state(ClientId::new(2), DsState::Faulted);

        let mut gw = up.guest.guest_work.lock().await;
        let ds = &mut up.downstairs;

        let gw_id = 19;
        let next_id = JobId(1010);

        // Create a flush, enqueue it on both the downstairs
        // and the guest work queues.
        let deps = ds.ds_active.deps_for_flush(next_id);
        let op = create_flush(
            next_id, deps, 22, // Flush number
            gw_id, 11, // Gen number
            None, None,
        );

        let new_gtos = GtoS::new(next_id, None, None);
        {
            gw.active.insert(gw_id, new_gtos);
        }
        drop(gw);

        ds.enqueue(op, ds_done_tx.clone()).await;
        assert!(ds.in_progress(next_id, ClientId::new(0)).is_some());

        ds.process_ds_completion(
            next_id,
            ClientId::new(0),
            Ok(vec![]),
            &UpstairsState::Active,
            None,
        )
        .unwrap();

        let ack_list = ds.ackable_work();
        assert_eq!(ack_list.len(), 1);

        // Simulation of what happens in up_ds_listen
        for ds_id_done in ack_list.iter() {
            assert_eq!(*ds_id_done, next_id);

            let done = ds.ds_active.get(ds_id_done).unwrap();
            assert_eq!(done.ack_status, AckStatus::AckReady);

            assert_eq!(done.guest_id, gw_id);
            ds.ack(*ds_id_done);

            assert!(ds.result(*ds_id_done).is_err());
        }
    }

    #[tokio::test]
    async fn flush_io_fail_and_skip() {
        // up_ds_listen test, a fail plus a skip on a flush will result in an
        // error back to the guest.
        let up = Upstairs::test_default(None);
        let (ds_done_tx, _ds_done_rx) = mpsc::channel(500);
        up.force_active().unwrap();
        for cid in ClientId::iter() {
            up.force_ds_state(cid, DsState::WaitActive);
            up.force_ds_state(cid, DsState::WaitQuorum);
            up.force_ds_state(cid, DsState::Active);
        }
        // Only DS 0 is faulted.
        up.force_ds_state(ClientId::new(0), DsState::Faulted);

        let mut gw = up.guest.guest_work.lock().await;
        let ds = &mut up.downstairs;

        let gw_id = 19;
        let next_id = JobId(1010);

        // Create a flush, enqueue it on both the downstairs
        // and the guest work queues.
        let deps = ds.ds_active.deps_for_flush(next_id);
        let op = create_flush(
            next_id, deps, 22, // Flush number
            gw_id, 11, // Gen number
            None, None,
        );

        let new_gtos = GtoS::new(next_id, None, None);
        {
            gw.active.insert(gw_id, new_gtos);
        }
        drop(gw);

        ds.enqueue(op, ds_done_tx.clone()).await;
        assert!(ds.in_progress(next_id, ClientId::new(1)).is_some());
        assert!(ds.in_progress(next_id, ClientId::new(2)).is_some());

        // DS 1 has a failure, and this won't return true as we don't
        // have enough success yet to ACK to the guest.
        assert!(!ds
            .process_ds_completion(
                next_id,
                ClientId::new(1),
                Err(CrucibleError::GenericError("bad".to_string())),
                &UpstairsState::Active,
                None,
            )
            .unwrap());

        // DS 2 as it's the final IO will indicate it is time to notify
        // the guest we have a result for them.
        assert!(ds
            .process_ds_completion(
                next_id,
                ClientId::new(2),
                Ok(vec![]),
                &UpstairsState::Active,
                None
            )
            .unwrap());

        let ack_list = ds.ackable_work();
        assert_eq!(ack_list.len(), 1);

        // Simulation of what happens in up_ds_listen
        for ds_id_done in ack_list.iter() {
            assert_eq!(*ds_id_done, next_id);

            let done = ds.ds_active.get(ds_id_done).unwrap();
            assert_eq!(done.ack_status, AckStatus::AckReady);

            assert_eq!(done.guest_id, gw_id);

            ds.ack(*ds_id_done);

            assert!(ds.result(*ds_id_done).is_err());
        }
    }

    // Deactivate tests
    #[tokio::test]
    async fn deactivate_after_work_completed_write() {
        deactivate_after_work_completed(false).await;
    }

    #[tokio::test]
    async fn deactivate_after_work_completed_write_unwritten() {
        deactivate_after_work_completed(true).await;
    }

    async fn deactivate_after_work_completed(is_write_unwritten: bool) {
        // Verify that submitted IO will continue after a deactivate.
        // Verify that the flush takes three completions.
        // Verify that deactivate done returns the upstairs to init.

        let up = make_upstairs();
        let (ds_done_tx, _ds_done_rx) = mpsc::channel(500);
        up.force_active().unwrap();
        let ds = &mut up.downstairs;
        up.force_ds_state(ClientId::new(0), DsState::Active);
        up.force_ds_state(ClientId::new(1), DsState::Active);
        up.force_ds_state(ClientId::new(2), DsState::Active);

        // Build a write, put it on the work queue.
        let id1 = ds.next_id();

        let (request, iblocks) = generic_write_request();
        let op = create_write_eob(
            &mut ds,
            id1,
            iblocks,
            10,
            vec![request],
            is_write_unwritten,
        );
        ds.enqueue(op, ds_done_tx.clone()).await;

        // Submit the writes
        assert!(ds.in_progress(id1, ClientId::new(0)).is_some());
        assert!(ds.in_progress(id1, ClientId::new(1)).is_some());
        assert!(ds.in_progress(id1, ClientId::new(2)).is_some());

        drop(ds);

        // Create and enqueue the flush by setting deactivate
        // The created flush should be the next ID
        up.set_deactivate(None, ds_done_tx.clone()).await.unwrap();
        let flush_id = JobId(id1.0 + 1);

        ds = up.downstairs.lock().await;
        // Complete the writes
        ds.process_ds_completion(
            id1,
            ClientId::new(0),
            Ok(vec![]),
            &UpstairsState::Active,
            None,
        )
        .unwrap();
        ds.process_ds_completion(
            id1,
            ClientId::new(1),
            Ok(vec![]),
            &UpstairsState::Active,
            None,
        )
        .unwrap();
        ds.process_ds_completion(
            id1,
            ClientId::new(2),
            Ok(vec![]),
            &UpstairsState::Active,
            None,
        )
        .unwrap();

        // Ack the writes to the guest.
        ds.ack(id1);

        // Send the flush created for us when we set deactivated to
        // the two downstairs.
        ds.in_progress(flush_id, ClientId::new(0));
        ds.in_progress(flush_id, ClientId::new(2));

        // Complete the flush on those downstairs.
        // One flush won't result in an ACK
        assert!(!ds
            .process_ds_completion(
                flush_id,
                ClientId::new(0),
                Ok(vec![]),
                &UpstairsState::Deactivating,
                None
            )
            .unwrap());

        // The 2nd ack when disconnecting still won't trigger an ack.
        assert!(!ds
            .process_ds_completion(
                flush_id,
                ClientId::new(2),
                Ok(vec![]),
                &UpstairsState::Deactivating,
                None
            )
            .unwrap());

        // Verify we can deactivate the completed DS
        drop(ds);
        assert!(up.ds_deactivate(ClientId::new(0)).await);
        assert!(up.ds_deactivate(ClientId::new(2)).await);

        // Verify the remaining DS can not deactivate
        assert!(!up.ds_deactivate(ClientId::new(1)).await);

        // Verify the deactivate is not done yet.
        up.deactivate_transition_check().await;
        assert!(up.is_deactivating().await);

        ds = up.downstairs.lock().await;
        // Make sure the correct DS have changed state.
        assert_eq!(ds.clients[ClientId::new(0)].state(), DsState::Deactivated);
        assert_eq!(ds.clients[ClientId::new(2)].state(), DsState::Deactivated);
        assert_eq!(ds.clients[ClientId::new(1)].state(), DsState::Active);

        // Send and complete the flush
        ds.in_progress(flush_id, ClientId::new(1));
        assert!(ds
            .process_ds_completion(
                flush_id,
                ClientId::new(1),
                Ok(vec![]),
                &UpstairsState::Deactivating,
                None
            )
            .unwrap());
        // Ack the flush..
        ds.ack(flush_id);

        drop(ds);
        assert!(up.ds_deactivate(ClientId::new(1)).await);

        // Report all three DS as missing, which moves them to New
        up.ds_missing(ClientId::new(0)).await;
        up.ds_missing(ClientId::new(1)).await;
        up.ds_missing(ClientId::new(2)).await;

        // Verify we have disconnected and can go back to init.
        up.deactivate_transition_check().await;
        assert!(!up.is_deactivating().await);

        // Verify after the ds_missing, all downstairs are New
        let ds = &up.downstairs;
        assert_eq!(ds.clients[ClientId::new(0)].state()(), DsState::New);
        assert_eq!(ds.clients[ClientId::new(1)].state()(), DsState::New);
        assert_eq!(ds.clients[ClientId::new(2)].state()(), DsState::New);
    }

    #[tokio::test]
    async fn deactivate_when_empty() {
        // Verify we can deactivate if no work is present, without
        // creating a flush (as their should already have been one).
        // Verify after all three downstairs are deactivated, we can
        // transition the upstairs back to init.

        let up = Upstairs::test_default(None);
        let (ds_done_tx, _ds_done_rx) = mpsc::channel(500);
        up.force_active().unwrap();
        up.force_ds_state(ClientId::new(0), DsState::Active);
        up.force_ds_state(ClientId::new(1), DsState::Active);
        up.force_ds_state(ClientId::new(2), DsState::Active);

        up.set_deactivate(None, ds_done_tx.clone()).await.unwrap();

        // Verify we can deactivate as there is no work
        assert!(up.ds_deactivate(ClientId::new(0)).await);
        assert!(up.ds_deactivate(ClientId::new(1)).await);
        assert!(up.ds_deactivate(ClientId::new(2)).await);

        let ds = &up.downstairs;
        // Make sure the correct DS have changed state.
        assert_eq!(
            ds.clients[ClientId::new(0)].state()(),
            DsState::Deactivated
        );
        assert_eq!(
            ds.clients[ClientId::new(1)].state()(),
            DsState::Deactivated
        );
        assert_eq!(
            ds.clients[ClientId::new(2)].state()(),
            DsState::Deactivated
        );
        drop(ds);

        // Mark all three DS as missing, which moves their state to New
        up.ds_missing(ClientId::new(0)).await;
        up.ds_missing(ClientId::new(1)).await;
        up.ds_missing(ClientId::new(2)).await;

        // Verify now we can go back to init.
        up.deactivate_transition_check().await;
        assert!(!up.is_deactivating().await);
    }

    #[tokio::test]
    async fn deactivate_not_without_flush_write() {
        deactivate_not_without_flush(false).await;
    }

    #[tokio::test]
    async fn deactivate_not_without_flush_write_unwritten() {
        deactivate_not_without_flush(true).await;
    }

    async fn deactivate_not_without_flush(is_write_unwritten: bool) {
        // Verify that we can't deactivate without a flush as the
        // last job on the list

        let up = make_upstairs();
        let (ds_done_tx, _ds_done_rx) = mpsc::channel(500);
        up.force_active().unwrap();
        let ds = &mut up.downstairs;
        up.force_ds_state(ClientId::new(0), DsState::Active);
        up.force_ds_state(ClientId::new(1), DsState::Active);
        up.force_ds_state(ClientId::new(2), DsState::Active);

        // Build a write, put it on the work queue.
        let id1 = ds.next_id();

        let (request, iblocks) = generic_write_request();
        let op = create_write_eob(
            &mut ds,
            id1,
            iblocks,
            10,
            vec![request],
            is_write_unwritten,
        );
        ds.enqueue(op, ds_done_tx.clone()).await;

        // Submit the writes
        assert!(ds.in_progress(id1, ClientId::new(0)).is_some());
        assert!(ds.in_progress(id1, ClientId::new(1)).is_some());
        assert!(ds.in_progress(id1, ClientId::new(2)).is_some());

        drop(ds);
        up.set_deactivate(None, ds_done_tx.clone()).await.unwrap();
        ds = up.downstairs.lock().await;

        // Complete the writes
        ds.process_ds_completion(
            id1,
            ClientId::new(0),
            Ok(vec![]),
            &UpstairsState::Deactivating,
            None,
        )
        .unwrap();
        ds.process_ds_completion(
            id1,
            ClientId::new(1),
            Ok(vec![]),
            &UpstairsState::Deactivating,
            None,
        )
        .unwrap();
        ds.process_ds_completion(
            id1,
            ClientId::new(2),
            Ok(vec![]),
            &UpstairsState::Deactivating,
            None,
        )
        .unwrap();

        // Ack the writes to the guest.
        ds.ack(id1);

        // Verify we will not transition to deactivated without a flush.
        drop(ds);
        assert!(!up.ds_deactivate(ClientId::new(0)).await);
        assert!(!up.ds_deactivate(ClientId::new(1)).await);
        assert!(!up.ds_deactivate(ClientId::new(2)).await);

        // Verify the deactivate is not done yet.
        up.deactivate_transition_check().await;
        assert!(up.is_deactivating().await);

        ds = up.downstairs.lock().await;
        // Make sure no DS have changed state.
        assert_eq!(ds.clients[ClientId::new(0)].state(), DsState::Active);
        assert_eq!(ds.clients[ClientId::new(2)].state(), DsState::Active);
        assert_eq!(ds.clients[ClientId::new(1)].state(), DsState::Active);
    }

    #[tokio::test]
    async fn deactivate_not_when_active() {
        // Verify that we can't set deactivate on the upstairs when
        // the upstairs is still in init.
        // Verify that we can't set deactivate on the upstairs when
        // we are deactivating.
        // TODO: This test should change when we support this behavior.

        let up = Upstairs::test_default(None);
        let (ds_done_tx, _ds_done_rx) = mpsc::channel(500);
        assert!(up.set_deactivate(None, ds_done_tx.clone()).await.is_err());
        up.force_active().unwrap();
        up.set_deactivate(None, ds_done_tx.clone()).await.unwrap();
        assert!(up.set_deactivate(None, ds_done_tx.clone()).await.is_err());
    }

    #[tokio::test]
    async fn deactivate_ds_not_when_active() {
        // No ds can deactivate when upstairs is not deactivating.

        let up = Upstairs::test_default(None);
        up.force_active().unwrap();
        let ds = &mut up.downstairs;
        up.force_ds_state(ClientId::new(0), DsState::Active);
        up.force_ds_state(ClientId::new(1), DsState::Active);
        up.force_ds_state(ClientId::new(2), DsState::Active);

        drop(ds);

        // Verify we cannot deactivate even when there is no work
        assert!(!up.ds_deactivate(ClientId::new(0)).await);
        assert!(!up.ds_deactivate(ClientId::new(1)).await);
        assert!(!up.ds_deactivate(ClientId::new(2)).await);

        ds = up.downstairs.lock().await;
        // Make sure no DS have changed state.
        assert_eq!(ds.clients[ClientId::new(0)].state(), DsState::Active);
        assert_eq!(ds.clients[ClientId::new(1)].state(), DsState::Active);
        assert_eq!(ds.clients[ClientId::new(2)].state(), DsState::Active);
    }

    #[tokio::test]
    async fn deactivate_ds_not_when_initializing() {
        // No deactivate of downstairs when upstairs not active.

        let up = Upstairs::test_default(None);

        // Verify we cannot deactivate before the upstairs is active
        assert!(!up.ds_deactivate(ClientId::new(0)).await);
        assert!(!up.ds_deactivate(ClientId::new(1)).await);
        assert!(!up.ds_deactivate(ClientId::new(2)).await);

        let ds = &up.downstairs;
        // Make sure no DS have changed state.
        assert_eq!(ds.clients[ClientId::new(0)].state(), DsState::New);
        assert_eq!(ds.clients[ClientId::new(1)].state(), DsState::New);
        assert_eq!(ds.clients[ClientId::new(2)].state(), DsState::New);
    }

    // TODO matt hiiii
    // Tests for rep_in_progress
    #[tokio::test]
    async fn reconcile_rep_in_progress_none() {
        // No repairs on the queue, should return None
        let up = Upstairs::test_default(None);
        let ds = &mut up.downstairs;
        up.force_ds_state(ClientId::new(0), DsState::Repair);
        up.force_ds_state(ClientId::new(1), DsState::Repair);
        up.force_ds_state(ClientId::new(2), DsState::Repair);
        let w = ds.rep_in_progress(ClientId::new(0));
        assert_eq!(w, None);
    }

    #[tokio::test]
    async fn reconcile_repair_workflow_not_repair() {
        // Verify that rep_in_progress will not give out work if a
        // downstairs is not in the correct state, and that it will
        // clear the work queue and mark other downstairs as failed.
        let up = Upstairs::test_default(None);
        let rep_id = ReconciliationId(0);
        {
            let ds = &mut up.downstairs;
            // Put a jobs on the todo list
            ds.reconcile_task_list.push_back(ReconcileIO::new(
                rep_id,
                Message::ExtentClose {
                    repair_id: rep_id,
                    extent_id: 1,
                },
            ));
            // A downstairs is not in Repair state
            up.force_ds_state(ClientId::new(0), DsState::Repair);
            up.force_ds_state(ClientId::new(1), DsState::WaitQuorum);
            up.force_ds_state(ClientId::new(2), DsState::Repair);
        }
        // Move that job to next to do.
        let nw = up.new_rec_work().await;
        assert!(nw.is_err());
        let ds = &mut up.downstairs;
        assert_eq!(ds.clients[ClientId::new(0)].state(), DsState::FailedRepair);
        assert_eq!(ds.clients[ClientId::new(1)].state(), DsState::WaitQuorum);
        assert_eq!(ds.clients[ClientId::new(2)].state(), DsState::FailedRepair);

        // Verify rep_in_progress now returns none for all DS
        assert!(ds.reconcile_task_list.is_empty());
        assert!(ds.rep_in_progress(ClientId::new(0)).is_none());
        assert!(ds.rep_in_progress(ClientId::new(1)).is_none());
        assert!(ds.rep_in_progress(ClientId::new(2)).is_none());
    }

    #[tokio::test]
    async fn reconcile_repair_workflow_not_repair_later() {
        // Verify that rep_done still works even after we have a downstairs
        // in the FailedRepair state. Verify that attempts to get new work
        // after a failed repair now return none.
        let up = Upstairs::test_default(None);
        let rep_id = ReconciliationId(0);
        {
            let ds = &mut up.downstairs;
            up.force_ds_state(ClientId::new(0), DsState::Repair);
            up.force_ds_state(ClientId::new(1), DsState::Repair);
            up.force_ds_state(ClientId::new(2), DsState::Repair);
            // Put two jobs on the todo list
            ds.reconcile_task_list.push_back(ReconcileIO::new(
                rep_id,
                Message::ExtentClose {
                    repair_id: rep_id,
                    extent_id: 1,
                },
            ));
        }
        // Move that job to next to do.
        let nw = up.new_rec_work().await;
        assert!(nw.unwrap());
        let ds = &mut up.downstairs;
        // Mark all three as in progress
        assert!(ds.rep_in_progress(ClientId::new(0)).is_some());
        assert!(ds.rep_in_progress(ClientId::new(1)).is_some());
        assert!(ds.rep_in_progress(ClientId::new(2)).is_some());

        // Now verify we can be done even if a DS is gone
        up.force_ds_state(ClientId::new(1), DsState::New);
        // Now, make sure we consider this done only after all three are done
        assert!(!ds.rep_done(ClientId::new(0), rep_id));
        assert!(!ds.rep_done(ClientId::new(1), rep_id));
        assert!(ds.rep_done(ClientId::new(2), rep_id));

        // Getting the next work to do should verify the previous is done,
        // and handle a state change for a downstairs.
        drop(ds);
        let nw = up.new_rec_work().await;
        assert!(nw.is_err());
        let ds = &mut up.downstairs;
        assert_eq!(ds.clients[ClientId::new(0)].state(), DsState::FailedRepair);
        assert_eq!(ds.clients[ClientId::new(1)].state(), DsState::New);
        assert_eq!(ds.clients[ClientId::new(2)].state(), DsState::FailedRepair);

        // Verify rep_in_progress now returns none for all DS
        assert!(ds.reconcile_task_list.is_empty());
        assert!(ds.rep_in_progress(ClientId::new(0)).is_none());
        assert!(ds.rep_in_progress(ClientId::new(1)).is_none());
        assert!(ds.rep_in_progress(ClientId::new(2)).is_none());
    }

    #[tokio::test]
    async fn reconcile_repair_workflow_repair_later() {
        // Verify that a downstairs not in repair mode will ignore new
        // work requests until it transitions to repair.
        let up = Upstairs::test_default(None);
        let rep_id = ReconciliationId(0);
        {
            up.force_ds_state(ClientId::new(0), DsState::Repair);
            up.force_ds_state(ClientId::new(1), DsState::Repair);
            up.force_ds_state(ClientId::new(2), DsState::Repair);
            // Put a job on the todo list
            let ds = &mut up.downstairs;
            ds.reconcile_task_list.push_back(ReconcileIO::new(
                rep_id,
                Message::ExtentClose {
                    repair_id: rep_id,
                    extent_id: 1,
                },
            ));
        }
        // Move that job to next to do.
        let nw = up.new_rec_work().await;
        assert!(nw.unwrap());
        let ds = &mut up.downstairs;
        // Mark all three as in progress
        assert!(ds.rep_in_progress(ClientId::new(0)).is_some());
        assert!(ds.rep_in_progress(ClientId::new(1)).is_some());
        up.force_ds_state(ClientId::new(2), DsState::New);
        assert!(ds.rep_in_progress(ClientId::new(2)).is_none());

        // Okay, now the DS is back and ready for repair, verify it will
        // start taking work.
        up.force_ds_state(ClientId::new(2), DsState::Repair);
        assert!(ds.rep_in_progress(ClientId::new(2)).is_some());
    }

    #[tokio::test]
    #[should_panic]
    async fn reconcile_rep_in_progress_bad1() {
        // Verify the same downstairs can't mark a job in progress twice
        let up = Upstairs::test_default(None);
        let rep_id = ReconciliationId(0);
        {
            up.force_ds_state(ClientId::new(0), DsState::Repair);
            up.force_ds_state(ClientId::new(1), DsState::Repair);
            up.force_ds_state(ClientId::new(2), DsState::Repair);
            // Put a job on the todo list
            let ds = &mut up.downstairs;
            ds.reconcile_task_list.push_back(ReconcileIO::new(
                rep_id,
                Message::ExtentClose {
                    repair_id: rep_id,
                    extent_id: 1,
                },
            ));
        }
        // Move that job to next to do.
        let _ = up.new_rec_work().await;
        let ds = &mut up.downstairs;
        assert!(ds.rep_in_progress(ClientId::new(0)).is_some());
        assert!(ds.rep_in_progress(ClientId::new(0)).is_some());
    }

    #[tokio::test]
    #[should_panic]
    async fn reconcile_rep_done_too_soon() {
        // Verify a job can't go new -> done
        let up = Upstairs::test_default(None);
        let rep_id = ReconciliationId(0);
        {
            up.force_ds_state(ClientId::new(0), DsState::Repair);
            up.force_ds_state(ClientId::new(1), DsState::Repair);
            up.force_ds_state(ClientId::new(2), DsState::Repair);
            // Put a job on the todo list
            let ds = &mut up.downstairs;
            ds.reconcile_task_list.push_back(ReconcileIO::new(
                rep_id,
                Message::ExtentClose {
                    repair_id: rep_id,
                    extent_id: 1,
                },
            ));
        }
        // Move that job to next to do.
        let _ = up.new_rec_work().await;
        let ds = &mut up.downstairs;
        ds.rep_done(ClientId::new(0), rep_id);
    }

    #[tokio::test]
    async fn reconcile_repair_workflow_1() {
        let up = Upstairs::test_default(None);
        let mut rep_id = ReconciliationId(0);
        {
            up.force_ds_state(ClientId::new(0), DsState::Repair);
            up.force_ds_state(ClientId::new(1), DsState::Repair);
            up.force_ds_state(ClientId::new(2), DsState::Repair);
            // Put two jobs on the todo list
            let ds = &mut up.downstairs;
            ds.reconcile_task_list.push_back(ReconcileIO::new(
                rep_id,
                Message::ExtentClose {
                    repair_id: rep_id,
                    extent_id: 1,
                },
            ));
            ds.reconcile_task_list.push_back(ReconcileIO::new(
                ReconciliationId(rep_id.0 + 1),
                Message::ExtentClose {
                    repair_id: rep_id,
                    extent_id: 1,
                },
            ));
        }
        // Move that job to next to do.
        let nw = up.new_rec_work().await;
        assert!(nw.unwrap());
        let ds = &mut up.downstairs;
        // Mark all three as in progress
        assert!(ds.rep_in_progress(ClientId::new(0)).is_some());
        assert!(ds.rep_in_progress(ClientId::new(1)).is_some());
        assert!(ds.rep_in_progress(ClientId::new(2)).is_some());

        // Now, make sure we consider this done only after all three are done
        assert!(!ds.rep_done(ClientId::new(0), rep_id));
        assert!(!ds.rep_done(ClientId::new(1), rep_id));
        assert!(ds.rep_done(ClientId::new(2), rep_id));

        // Getting the next work to do should verify the previous is done
        drop(ds);
        let nw = up.new_rec_work().await;
        assert!(nw.unwrap());
        let ds = &mut up.downstairs;
        // Mark all three as in progress
        assert!(ds.rep_in_progress(ClientId::new(0)).is_some());
        assert!(ds.rep_in_progress(ClientId::new(1)).is_some());
        assert!(ds.rep_in_progress(ClientId::new(2)).is_some());

        // Now, make sure we consider this done only after all three are done
        rep_id.0 += 1;
        assert!(!ds.rep_done(ClientId::new(0), rep_id));
        assert!(!ds.rep_done(ClientId::new(1), rep_id));
        assert!(ds.rep_done(ClientId::new(2), rep_id));

        drop(ds);
        // Now, we should be empty, so nw is false
        assert!(!up.new_rec_work().await.unwrap());
    }

    #[tokio::test]
    #[should_panic]
    async fn reconcile_leave_no_job_behind() {
        // Verify we can't start a new job before the old is finished.
        let up = Upstairs::test_default(None);
        let rep_id = ReconciliationId(0);
        {
            up.force_ds_state(ClientId::new(0), DsState::Repair);
            up.force_ds_state(ClientId::new(1), DsState::Repair);
            up.force_ds_state(ClientId::new(2), DsState::Repair);
            // Put two jobs on the todo list
            let ds = &mut up.downstairs;
            ds.reconcile_task_list.push_back(ReconcileIO::new(
                rep_id,
                Message::ExtentClose {
                    repair_id: rep_id,
                    extent_id: 1,
                },
            ));
            ds.reconcile_task_list.push_back(ReconcileIO::new(
                ReconciliationId(rep_id.0 + 1),
                Message::ExtentClose {
                    repair_id: rep_id,
                    extent_id: 1,
                },
            ));
        }
        // Move that job to next to do.
        let nw = up.new_rec_work().await;
        assert!(nw.unwrap());
        let ds = &mut up.downstairs;
        // Mark all three as in progress
        assert!(ds.rep_in_progress(ClientId::new(0)).is_some());
        assert!(ds.rep_in_progress(ClientId::new(1)).is_some());
        assert!(ds.rep_in_progress(ClientId::new(2)).is_some());

        // Now, make sure we consider this done only after all three are done
        assert!(!ds.rep_done(ClientId::new(0), rep_id));
        assert!(!ds.rep_done(ClientId::new(1), rep_id));
        // don't finish

        // Getting the next work to do should verify the previous is done
        drop(ds);
        let nw = up.new_rec_work().await;
        assert!(nw.unwrap());
    }

    #[tokio::test]
    async fn reconcile_repair_workflow_2() {
        // Verify Done or Skipped works for rep_done
        let up = Upstairs::test_default(None);
        let rep_id = ReconciliationId(0);
        {
            up.force_ds_state(ClientId::new(0), DsState::Repair);
            up.force_ds_state(ClientId::new(1), DsState::Repair);
            up.force_ds_state(ClientId::new(2), DsState::Repair);
            // Put a job on the todo list
            let ds = &mut up.downstairs;
            ds.reconcile_task_list.push_back(ReconcileIO::new(
                rep_id,
                Message::ExtentClose {
                    repair_id: rep_id,
                    extent_id: 1,
                },
            ));
        }
        // Move that job to next to do.
        let nw = up.new_rec_work().await;
        assert!(nw.unwrap());
        let ds = &mut up.downstairs;
        // Mark all three as in progress
        assert!(ds.rep_in_progress(ClientId::new(0)).is_some());
        if let Some(job) = &mut ds.reconcile_current_work {
            let oldstate = job.state.insert(ClientId::new(1), IOState::Skipped);
            assert_eq!(oldstate, IOState::New);
        } else {
            panic!("Failed to find next task");
        }

        assert!(ds.rep_in_progress(ClientId::new(2)).is_some());

        // Now, make sure we consider this done only after all three are done
        assert!(!ds.rep_done(ClientId::new(0), rep_id));
        // This should panic: assert!(!ds.rep_done(1, rep_id));
        assert!(ds.rep_done(ClientId::new(2), rep_id));
    }

    #[tokio::test]
    #[should_panic]
    async fn reconcile_repair_inprogress_not_done() {
        // Verify Done or Skipped works for rep_done
        let up = Upstairs::test_default(None);
        let rep_id = ReconciliationId(0);
        {
            up.force_ds_state(ClientId::new(0), DsState::Repair);
            up.force_ds_state(ClientId::new(1), DsState::Repair);
            up.force_ds_state(ClientId::new(2), DsState::Repair);
            // Put a job on the todo list
            let ds = &mut up.downstairs;
            ds.reconcile_task_list.push_back(ReconcileIO::new(
                rep_id,
                Message::ExtentClose {
                    repair_id: rep_id,
                    extent_id: 1,
                },
            ));
        }
        // Move that job to next to do.
        let nw = up.new_rec_work().await;
        assert!(nw.unwrap());
        let ds = &mut up.downstairs;
        // Mark one as skipped
        if let Some(job) = &mut ds.reconcile_current_work {
            let oldstate = job.state.insert(ClientId::new(1), IOState::Skipped);
            assert_eq!(oldstate, IOState::New);
        } else {
            panic!("Failed to find next task");
        }

        // Can't mark done a skipped job
        ds.rep_done(ClientId::new(1), rep_id);
    }

    #[tokio::test]
    #[should_panic]
    async fn reconcile_repair_workflow_too_soon() {
        // Verify that jobs must be in progress before done.
        let up = Upstairs::test_default(None);
        let rep_id = ReconciliationId(0);
        {
            up.force_ds_state(ClientId::new(0), DsState::Repair);
            up.force_ds_state(ClientId::new(1), DsState::Repair);
            up.force_ds_state(ClientId::new(2), DsState::Repair);
            // Put a job on the todo list
            let ds = &mut up.downstairs;
            ds.reconcile_task_list.push_back(ReconcileIO::new(
                rep_id,
                Message::ExtentClose {
                    repair_id: rep_id,
                    extent_id: 1,
                },
            ));
        }
        // Move that job to next to do.
        let nw = up.new_rec_work().await;
        assert!(nw.unwrap());
        let ds = &mut up.downstairs;
        // Jump straight to done.
        // Now, make sure we consider this done only after all three are done
        ds.rep_done(ClientId::new(0), rep_id);
    }

    #[tokio::test]
    async fn test_no_iop_limit() -> Result<()> {
        let guest = Guest::new();

        assert!(guest.consume_req().await.is_none());

        // Don't use guest.read, that will send a block size query that will
        // never be answered.
        let _ = guest
            .send(BlockOp::Read {
                offset: Block::new_512(0),
                data: Buffer::new(1),
            })
            .await;
        let _ = guest
            .send(BlockOp::Read {
                offset: Block::new_512(0),
                data: Buffer::new(8000),
            })
            .await;
        let _ = guest
            .send(BlockOp::Read {
                offset: Block::new_512(0),
                data: Buffer::new(16000),
            })
            .await;

        // With no IOP limit, all requests are consumed immediately
        assert!(guest.consume_req().await.is_some());
        assert!(guest.consume_req().await.is_some());
        assert!(guest.consume_req().await.is_some());

        assert!(guest.consume_req().await.is_none());

        // If no IOP limit set, don't track it
        assert_eq!(*guest.iop_tokens.lock().unwrap(), 0);

        Ok(())
    }

    #[tokio::test]
    async fn test_set_iop_limit() -> Result<()> {
        let mut guest = Guest::new();
        guest.set_iop_limit(16000, 2);

        assert!(guest.consume_req().await.is_none());

        // Don't use guest.read, that will send a block size query that will
        // never be answered.
        let _ = guest
            .send(BlockOp::Read {
                offset: Block::new_512(0),
                data: Buffer::new(1),
            })
            .await;
        let _ = guest
            .send(BlockOp::Read {
                offset: Block::new_512(0),
                data: Buffer::new(8000),
            })
            .await;
        let _ = guest
            .send(BlockOp::Read {
                offset: Block::new_512(0),
                data: Buffer::new(16000),
            })
            .await;

        // First two reads succeed
        assert!(guest.consume_req().await.is_some());
        assert!(guest.consume_req().await.is_some());

        // Next cannot be consumed until there's available IOP tokens so it
        // remains in the queue.
        assert!(guest.consume_req().await.is_none());
        assert!(!guest.reqs.lock().await.is_empty());
        assert_eq!(*guest.iop_tokens.lock().unwrap(), 2);

        // Replenish one token, meaning next read can be consumed
        guest.leak_iop_tokens(1);
        assert_eq!(*guest.iop_tokens.lock().unwrap(), 1);

        assert!(guest.consume_req().await.is_some());
        assert!(guest.reqs.lock().await.is_empty());
        assert_eq!(*guest.iop_tokens.lock().unwrap(), 2);

        guest.leak_iop_tokens(2);
        assert_eq!(*guest.iop_tokens.lock().unwrap(), 0);

        guest.leak_iop_tokens(16000);
        assert_eq!(*guest.iop_tokens.lock().unwrap(), 0);

        Ok(())
    }

    #[tokio::test]
    async fn test_flush_does_not_consume_iops() -> Result<()> {
        let mut guest = Guest::new();

        // Set 0 as IOP limit
        guest.set_iop_limit(16000, 0);
        assert!(guest.consume_req().await.is_none());

        let _ = guest
            .send(BlockOp::Flush {
                snapshot_details: None,
            })
            .await;
        let _ = guest
            .send(BlockOp::Flush {
                snapshot_details: None,
            })
            .await;
        let _ = guest
            .send(BlockOp::Flush {
                snapshot_details: None,
            })
            .await;

        assert!(guest.consume_req().await.is_some());
        assert!(guest.consume_req().await.is_some());
        assert!(guest.consume_req().await.is_some());

        assert!(guest.consume_req().await.is_none());

        Ok(())
    }

    #[tokio::test]
    async fn test_set_bw_limit() -> Result<()> {
        let mut guest = Guest::new();
        guest.set_bw_limit(1024 * 1024); // 1 KiB

        assert!(guest.consume_req().await.is_none());

        // Don't use guest.read, that will send a block size query that will
        // never be answered.
        let _ = guest
            .send(BlockOp::Read {
                offset: Block::new_512(0),
                data: Buffer::new(1024 * 1024 / 2),
            })
            .await;
        let _ = guest
            .send(BlockOp::Read {
                offset: Block::new_512(0),
                data: Buffer::new(1024 * 1024 / 2),
            })
            .await;
        let _ = guest
            .send(BlockOp::Read {
                offset: Block::new_512(0),
                data: Buffer::new(1024 * 1024 / 2),
            })
            .await;

        // First two reads succeed
        assert!(guest.consume_req().await.is_some());
        assert!(guest.consume_req().await.is_some());

        // Next cannot be consumed until there's available BW tokens so it
        // remains in the queue.
        assert!(guest.consume_req().await.is_none());
        assert!(!guest.reqs.lock().await.is_empty());
        assert_eq!(*guest.bw_tokens.lock().unwrap(), 1024 * 1024);

        // Replenish enough tokens, meaning next read can be consumed
        guest.leak_bw_tokens(1024 * 1024 / 2);
        assert_eq!(*guest.bw_tokens.lock().unwrap(), 1024 * 1024 / 2);

        assert!(guest.consume_req().await.is_some());
        assert!(guest.reqs.lock().await.is_empty());
        assert_eq!(*guest.bw_tokens.lock().unwrap(), 1024 * 1024);

        guest.leak_bw_tokens(1024 * 1024);
        assert_eq!(*guest.bw_tokens.lock().unwrap(), 0);

        guest.leak_bw_tokens(1024 * 1024 * 1024);
        assert_eq!(*guest.bw_tokens.lock().unwrap(), 0);

        Ok(())
    }

    #[tokio::test]
    async fn test_flush_does_not_consume_bw() -> Result<()> {
        let mut guest = Guest::new();

        // Set 0 as bandwidth limit
        guest.set_bw_limit(0);
        assert!(guest.consume_req().await.is_none());

        let _ = guest
            .send(BlockOp::Flush {
                snapshot_details: None,
            })
            .await;
        let _ = guest
            .send(BlockOp::Flush {
                snapshot_details: None,
            })
            .await;
        let _ = guest
            .send(BlockOp::Flush {
                snapshot_details: None,
            })
            .await;

        assert!(guest.consume_req().await.is_some());
        assert!(guest.consume_req().await.is_some());
        assert!(guest.consume_req().await.is_some());

        assert!(guest.consume_req().await.is_none());

        Ok(())
    }

    #[tokio::test]
    async fn test_iop_and_bw_limit() -> Result<()> {
        let mut guest = Guest::new();

        guest.set_iop_limit(16384, 500); // 1 IOP is 16 KiB
        guest.set_bw_limit(6400 * 1024); // 16384 B * 400 = 6400 KiB/s
        assert!(guest.consume_req().await.is_none());

        // Don't use guest.read, that will send a block size query that will
        // never be answered.

        // Validate that BW limit activates by sending two 7000 KiB IOs. 7000
        // KiB is only 437.5 IOPs

        let _ = guest
            .send(BlockOp::Read {
                offset: Block::new_512(0),
                data: Buffer::new(7000 * 1024),
            })
            .await;
        let _ = guest
            .send(BlockOp::Read {
                offset: Block::new_512(0),
                data: Buffer::new(7000 * 1024),
            })
            .await;

        assert!(guest.consume_req().await.is_some());
        assert!(guest.consume_req().await.is_none());

        // Assert we've hit the BW limit before IOPS
        assert_eq!(*guest.iop_tokens.lock().unwrap(), 438); // 437.5 rounded up
        assert_eq!(*guest.bw_tokens.lock().unwrap(), 7000 * 1024);

        guest.leak_iop_tokens(438);
        guest.leak_bw_tokens(7000 * 1024);

        assert!(guest.consume_req().await.is_some());
        assert!(guest.reqs.lock().await.is_empty());

        // Back to zero
        guest.leak_iop_tokens(438);
        guest.leak_bw_tokens(7000 * 1024);

        assert_eq!(*guest.iop_tokens.lock().unwrap(), 0);
        assert_eq!(*guest.bw_tokens.lock().unwrap(), 0);

        // Validate that IOP limit activates by sending 501 1024b IOs
        for _ in 0..500 {
            let _ = guest
                .send(BlockOp::Read {
                    offset: Block::new_512(0),
                    data: Buffer::new(1024),
                })
                .await;
            assert!(guest.consume_req().await.is_some());
        }

        let _ = guest
            .send(BlockOp::Read {
                offset: Block::new_512(0),
                data: Buffer::new(1024),
            })
            .await;
        assert!(guest.consume_req().await.is_none());

        // Assert we've hit the IOPS limit
        assert_eq!(*guest.iop_tokens.lock().unwrap(), 500);
        assert_eq!(*guest.bw_tokens.lock().unwrap(), 500 * 1024);

        // Back to zero
        guest.leak_iop_tokens(500);
        guest.leak_bw_tokens(500 * 1024);
        guest.reqs.lock().await.clear();

        assert!(guest.reqs.lock().await.is_empty());
        assert_eq!(*guest.iop_tokens.lock().unwrap(), 0);
        assert_eq!(*guest.bw_tokens.lock().unwrap(), 0);

        // From
        // https://aws.amazon.com/premiumsupport/knowledge-center/ebs-calculate-optimal-io-size/:
        //
        // Amazon EBS calculates the optimal I/O size using the following
        // equation: throughput / number of IOPS = optimal I/O size.

        let optimal_io_size: usize = 6400 * 1024 / 500;

        // Make sure this is <= an IOP size
        assert!(optimal_io_size <= 16384);

        // I mean, it makes sense: now we submit 500 of those to reach both
        // limits at the same time.
        for i in 0..500 {
            assert_eq!(*guest.iop_tokens.lock().unwrap(), i);
            assert_eq!(*guest.bw_tokens.lock().unwrap(), i * optimal_io_size);

            let _ = guest
                .send(BlockOp::Read {
                    offset: Block::new_512(0),
                    data: Buffer::new(optimal_io_size),
                })
                .await;

            assert!(guest.consume_req().await.is_some());
        }

        assert_eq!(*guest.iop_tokens.lock().unwrap(), 500);
        assert_eq!(*guest.bw_tokens.lock().unwrap(), 500 * optimal_io_size);

        Ok(())
    }

    // Is it possible to submit an IO that will never be sent? It shouldn't be!
    #[tokio::test]
    async fn test_impossible_io() -> Result<()> {
        let mut guest = Guest::new();

        guest.set_iop_limit(1024 * 1024 / 2, 10); // 1 IOP is half a KiB
        guest.set_bw_limit(1024 * 1024); // 1 KiB
        assert!(guest.consume_req().await.is_none());

        // Sending an IO of 10 KiB is larger than the bandwidth limit and
        // represents 20 IOPs, larger than the IOP limit.
        let _ = guest
            .send(BlockOp::Read {
                offset: Block::new_512(0),
                data: Buffer::new(10 * 1024 * 1024),
            })
            .await;
        let _ = guest
            .send(BlockOp::Read {
                offset: Block::new_512(0),
                data: Buffer::new(0),
            })
            .await;

        assert_eq!(*guest.iop_tokens.lock().unwrap(), 0);
        assert_eq!(*guest.bw_tokens.lock().unwrap(), 0);

        // Even though the first IO is larger than the bandwidth and IOP limit,
        // it should still succeed. The next IO should not, even if it consumes
        // nothing, because the iops and bw tokens will be larger than the limit
        // for a while (until they leak enough).

        assert!(guest.consume_req().await.is_some());
        assert!(guest.consume_req().await.is_none());

        assert_eq!(*guest.iop_tokens.lock().unwrap(), 20);
        assert_eq!(*guest.bw_tokens.lock().unwrap(), 10 * 1024 * 1024);

        // Bandwidth trigger is going to be larger and need more leaking to get
        // down to a point where the zero sized IO can fire.
        for _ in 0..9 {
            guest.leak_iop_tokens(10);
            guest.leak_bw_tokens(1024 * 1024);

            assert!(guest.consume_req().await.is_none());
        }

        assert_eq!(*guest.iop_tokens.lock().unwrap(), 0);
        assert_eq!(*guest.bw_tokens.lock().unwrap(), 1024 * 1024);

        assert!(guest.consume_req().await.is_none());

        guest.leak_iop_tokens(10);
        guest.leak_bw_tokens(1024 * 1024);

        // We've leaked 10 KiB worth, it should fire now!
        assert!(guest.consume_req().await.is_some());

        Ok(())
    }

    #[tokio::test]
    async fn work_writes_bad() {
        // Verify that three bad writes will ACK the IO, and set the
        // downstairs clients to failed.
        // This test also makes sure proper mutex behavior is used in
        // process_ds_operation.
        let up = Upstairs::test_default(None);
        let (ds_done_tx, _ds_done_rx) = mpsc::channel(500);
        for cid in ClientId::iter() {
            up.force_ds_state(cid, DsState::WaitActive);
            up.force_ds_state(cid, DsState::WaitQuorum);
            up.force_ds_state(cid, DsState::Active);
        }
        up.force_active().unwrap();

        let next_id = {
            let ds = &mut up.downstairs;

            let next_id = ds.next_id();

            let (request, iblocks) = generic_write_request();
            let op = create_write_eob(
                &mut ds,
                next_id,
                iblocks,
                10,
                vec![request],
                false,
            );

            ds.enqueue(op, ds_done_tx.clone()).await;

            assert!(ds.in_progress(next_id, ClientId::new(0)).is_some());
            assert!(ds.in_progress(next_id, ClientId::new(1)).is_some());
            assert!(ds.in_progress(next_id, ClientId::new(2)).is_some());

            next_id
        };

        // Set the error that everyone will use.
        let response = Err(CrucibleError::GenericError("bad".to_string()));

        // Process the operation for client 0
        assert!(!up
            .process_ds_operation(
                next_id,
                ClientId::new(0),
                response.clone(),
                None
            )
            .await
            .unwrap(),);
        // client 0 is failed, the others should be okay still
        assert_eq!(up.ds_state(ClientId::new(0)).await, DsState::Faulted);
        assert_eq!(up.ds_state(ClientId::new(1)).await, DsState::Active);
        assert_eq!(up.ds_state(ClientId::new(2)).await, DsState::Active);

        // Process the operation for client 1
        assert!(!up
            .process_ds_operation(
                next_id,
                ClientId::new(1),
                response.clone(),
                None
            )
            .await
            .unwrap(),);
        assert_eq!(up.ds_state(ClientId::new(0)).await, DsState::Faulted);
        assert_eq!(up.ds_state(ClientId::new(1)).await, DsState::Faulted);
        assert_eq!(up.ds_state(ClientId::new(2)).await, DsState::Active);

        // Three failures, But since this is a write we already have marked
        // the ACK as ready.
        // Process the operation for client 2
        assert!(!up
            .process_ds_operation(next_id, ClientId::new(2), response, None)
            .await
            .unwrap());
        assert_eq!(up.ds_state(ClientId::new(0)).await, DsState::Faulted);
        assert_eq!(up.ds_state(ClientId::new(1)).await, DsState::Faulted);
        assert_eq!(up.ds_state(ClientId::new(2)).await, DsState::Faulted);

        // Verify we can still ack this (failed) work
        let ds = &mut up.downstairs;
        assert_eq!(ds.ackable_work().len(), 1);
    }

    #[tokio::test]
    async fn read_after_write_fail_is_alright() {
        // Verify that if a single write fails on a downstairs, reads can still
        // be acked.
        //
        // Verify after acking IOs, we can then send a flush and
        // clear the jobs (some now failed/skipped) from the work queue.
        let up = Upstairs::test_default(None);
        let (ds_done_tx, _ds_done_rx) = mpsc::channel(500);
        for cid in ClientId::iter() {
            up.force_ds_state(cid, DsState::WaitActive);
            up.force_ds_state(cid, DsState::WaitQuorum);
            up.force_ds_state(cid, DsState::Active);
        }
        up.force_active().unwrap();

        // Create the write that fails on one DS
        let next_id = {
            let ds = &mut up.downstairs;

            let next_id = ds.next_id();

            let (request, iblocks) = generic_write_request();
            let op = create_write_eob(
                &mut ds,
                next_id,
                iblocks,
                10,
                vec![request],
                false,
            );

            ds.enqueue(op, ds_done_tx.clone()).await;

            ds.in_progress(next_id, ClientId::new(0));
            ds.in_progress(next_id, ClientId::new(1));
            ds.in_progress(next_id, ClientId::new(2));

            next_id
        };

        // Set the error that everyone will use.
        let err_response = Err(CrucibleError::GenericError("bad".to_string()));

        // Process the error operation for client 0
        assert!(!up
            .process_ds_operation(next_id, ClientId::new(0), err_response, None)
            .await
            .unwrap());
        // client 0 should be marked failed.
        assert_eq!(up.ds_state(ClientId::new(0)).await, DsState::Faulted);

        let ok_response = Ok(vec![]);
        // Process the good operation for client 1
        assert!(!up
            .process_ds_operation(
                next_id,
                ClientId::new(1),
                ok_response.clone(),
                None
            )
            .await
            .unwrap());

        // process_ds_operation should return true after we process this.
        assert!(up
            .process_ds_operation(next_id, ClientId::new(2), ok_response, None)
            .await
            .unwrap());
        assert_eq!(up.ds_state(ClientId::new(0)).await, DsState::Faulted);
        assert_eq!(up.ds_state(ClientId::new(1)).await, DsState::Active);
        assert_eq!(up.ds_state(ClientId::new(2)).await, DsState::Active);

        // Verify we can ack this work, then ack it.
        assert_eq!(up.downstairs.lock().await.ackable_work().len(), 1);
        up.downstairs.lock().await.ack(next_id);

        // Now, do a read.
        let (request, iblocks) = generic_read_request();

        let next_id = {
            let ds = &mut up.downstairs;

            let next_id = ds.next_id();
            let op = create_read_eob(
                &mut ds,
                next_id,
                iblocks,
                10,
                vec![request.clone()],
            );

            ds.enqueue(op, ds_done_tx.clone()).await;

            // As this DS is failed, it should return none
            assert_eq!(ds.in_progress(next_id, ClientId::new(0)), None);

            assert!(ds.in_progress(next_id, ClientId::new(1)).is_some());
            assert!(ds.in_progress(next_id, ClientId::new(2)).is_some());

            // We should have one job on the skipped job list for failed DS
            assert_eq!(ds.clients[ClientId::new(0)].skipped_jobs.len(), 1);
            assert_eq!(ds.clients[ClientId::new(1)].skipped_jobs.len(), 0);
            assert_eq!(ds.clients[ClientId::new(2)].skipped_jobs.len(), 0);

            next_id
        };

        let response =
            Ok(vec![ReadResponse::from_request_with_data(&request, &[])]);

        // Process the operation for client 1 this should return true
        assert!(up
            .process_ds_operation(
                next_id,
                ClientId::new(1),
                response.clone(),
                None
            )
            .await
            .unwrap(),);

        // Process the operation for client 2 this should return false
        assert!(!up
            .process_ds_operation(next_id, ClientId::new(2), response, None)
            .await
            .unwrap());

        // Verify we can ack this work, then ack it.
        assert_eq!(up.downstairs.lock().await.ackable_work().len(), 1);
        up.downstairs.lock().await.ack(next_id);

        // Perform the flush.
        let next_id = {
            let ds = &mut up.downstairs;

            let next_id = ds.next_id();
            let dep = ds.ds_active.deps_for_flush(next_id);
            let op = create_flush(next_id, dep, 10, 0, 0, None, None);
            ds.enqueue(op, ds_done_tx.clone()).await;

            // As this DS is failed, it should return none
            assert_eq!(ds.in_progress(next_id, ClientId::new(0)), None);
            assert!(ds.in_progress(next_id, ClientId::new(1)).is_some());
            assert!(ds.in_progress(next_id, ClientId::new(2)).is_some());

            next_id
        };

        let ok_response = Ok(vec![]);
        // Process the operation for client 1
        assert!(!up
            .process_ds_operation(
                next_id,
                ClientId::new(1),
                ok_response.clone(),
                None
            )
            .await
            .unwrap(),);

        // process_ds_operation should return true after we process this.
        assert!(up
            .process_ds_operation(next_id, ClientId::new(2), ok_response, None)
            .await
            .unwrap());

        // ACK the flush and let retire_check move things along.
        assert_eq!(up.downstairs.lock().await.ackable_work().len(), 1);
        up.downstairs.lock().await.ack(next_id);
        up.downstairs.lock().await.retire_check(next_id);

        let ds = &mut up.downstairs;
        assert_eq!(ds.ackable_work().len(), 0);

        // The write, the read, and now the flush should be completed.
        assert_eq!(ds.completed().len(), 3);

        // The last skipped flush should still be on the skipped list
        assert_eq!(ds.clients[ClientId::new(0)].skipped_jobs.len(), 1);
        assert!(ds.clients[ClientId::new(0)]
            .skipped_jobs
            .contains(&JobId(1002)));
        assert_eq!(ds.clients[ClientId::new(1)].skipped_jobs.len(), 0);
        assert_eq!(ds.clients[ClientId::new(2)].skipped_jobs.len(), 0);
    }

    #[tokio::test]
    async fn read_after_two_write_fail_is_alright() {
        // Verify that if two writes fail, a read can still be acked.
        let up = Upstairs::test_default(None);
        let (ds_done_tx, _ds_done_rx) = mpsc::channel(500);
        for cid in ClientId::iter() {
            up.force_ds_state(cid, DsState::WaitActive);
            up.force_ds_state(cid, DsState::WaitQuorum);
            up.force_ds_state(cid, DsState::Active);
        }
        up.force_active().unwrap();

        // Create the write that fails on two DS
        let next_id = {
            let ds = &mut up.downstairs;

            let next_id = ds.next_id();

            let (request, iblocks) = generic_write_request();
            let op = create_write_eob(
                &mut ds,
                next_id,
                iblocks,
                10,
                vec![request],
                false,
            );

            ds.enqueue(op, ds_done_tx.clone()).await;

            ds.in_progress(next_id, ClientId::new(0));
            ds.in_progress(next_id, ClientId::new(1));
            ds.in_progress(next_id, ClientId::new(2));

            next_id
        };

        // Set the error that everyone will use.
        let err_response = Err(CrucibleError::GenericError("bad".to_string()));

        // Process the operation for client 0
        assert!(!up
            .process_ds_operation(
                next_id,
                ClientId::new(0),
                err_response.clone(),
                None
            )
            .await
            .unwrap());
        // client 0 is failed, the others should be okay still
        assert_eq!(up.ds_state(ClientId::new(0)).await, DsState::Faulted);
        assert_eq!(up.ds_state(ClientId::new(1)).await, DsState::Active);
        assert_eq!(up.ds_state(ClientId::new(2)).await, DsState::Active);

        // Process the operation for client 1
        assert!(!up
            .process_ds_operation(next_id, ClientId::new(1), err_response, None)
            .await
            .unwrap());
        assert_eq!(up.ds_state(ClientId::new(0)).await, DsState::Faulted);
        assert_eq!(up.ds_state(ClientId::new(1)).await, DsState::Faulted);
        assert_eq!(up.ds_state(ClientId::new(2)).await, DsState::Active);

        let ok_response = Ok(vec![]);
        // Because we ACK writes, this op will always return false
        assert!(!up
            .process_ds_operation(next_id, ClientId::new(2), ok_response, None)
            .await
            .unwrap());
        assert_eq!(up.ds_state(ClientId::new(0)).await, DsState::Faulted);
        assert_eq!(up.ds_state(ClientId::new(1)).await, DsState::Faulted);
        assert_eq!(up.ds_state(ClientId::new(2)).await, DsState::Active);

        // Verify we can ack this work
        assert_eq!(up.downstairs.lock().await.ackable_work().len(), 1);

        // Now, do a read.

        let (request, iblocks) = generic_read_request();
        let next_id = {
            let ds = &mut up.downstairs;

            let next_id = ds.next_id();

            let op = create_read_eob(
                &mut ds,
                next_id,
                iblocks,
                10,
                vec![request.clone()],
            );

            ds.enqueue(op, ds_done_tx.clone()).await;

            // As this DS is failed, it should return none
            assert_eq!(ds.in_progress(next_id, ClientId::new(0)), None);
            assert_eq!(ds.in_progress(next_id, ClientId::new(1)), None);
            assert!(ds.in_progress(next_id, ClientId::new(2)).is_some());

            // Two downstairs should have a skipped job on their lists.
            assert_eq!(ds.clients[ClientId::new(0)].skipped_jobs.len(), 1);
            assert_eq!(ds.clients[ClientId::new(1)].skipped_jobs.len(), 1);
            assert_eq!(ds.clients[ClientId::new(2)].skipped_jobs.len(), 0);

            next_id
        };

        let response =
            Ok(vec![ReadResponse::from_request_with_data(&request, &[])]);

        // Process the operation for client 1 this should return true
        assert!(up
            .process_ds_operation(next_id, ClientId::new(2), response, None)
            .await
            .unwrap());
    }

    // Test function to Create a write, enqueue it, return the next_id used
    // for that write.  If make_in_progress is true, move all three downstairs
    // jobs to InProgress.
    async fn enqueue_write(
        up: &Arc<Upstairs>,
        make_in_progress: bool,
        ds_done_tx: mpsc::Sender<()>,
    ) -> JobId {
        let ds = &mut up.downstairs;

        let id = ds.next_id();

        let (request, iblocks) = generic_write_request();
        let op =
            create_write_eob(&mut ds, id, iblocks, 10, vec![request], false);
        ds.enqueue(op, ds_done_tx.clone()).await;

        if make_in_progress {
            ds.in_progress(id, ClientId::new(0));
            ds.in_progress(id, ClientId::new(1));
            ds.in_progress(id, ClientId::new(2));
        }

        id
    }

    // Test function to create and enqueue a flush.  Return the next_id used
    // for that flush.  If make_in_progress is true, move all three downstairs
    // jobs to InProgress.
    async fn enqueue_flush(
        up: &Arc<Upstairs>,
        make_in_progress: bool,
        ds_done_tx: mpsc::Sender<()>,
    ) -> JobId {
        let ds = &mut up.downstairs;

        let id = ds.next_id();

        let deps = ds.ds_active.deps_for_flush(id);
        let op = create_flush(id, deps, 10, 0, 0, None, None);
        ds.enqueue(op, ds_done_tx.clone()).await;

        if make_in_progress {
            ds.in_progress(id, ClientId::new(0));
            ds.in_progress(id, ClientId::new(1));
            ds.in_progress(id, ClientId::new(2));
        }

        id
    }

    // Test function to create and enqueue a (provided) read request.
    // Return the ID of the job created.
    async fn enqueue_read(
        up: &mut Upstairs,
        request: ReadRequest,
        iblocks: ImpactedBlocks,
        make_in_progress: bool,
        ds_done_tx: mpsc::Sender<()>,
    ) -> JobId {
        let ds = &mut up.downstairs;

        let read_id = ds.next_id();

        let op = create_read_eob(&mut ds, read_id, iblocks, 10, vec![request]);

        // Add the reads
        ds.enqueue(op, ds_done_tx.clone()).await;

        if make_in_progress {
            ds.in_progress(read_id, ClientId::new(0));
            ds.in_progress(read_id, ClientId::new(1));
            ds.in_progress(read_id, ClientId::new(2));
        }

        read_id
    }

    #[tokio::test]
    async fn send_io_live_repair_read() {
        // Check the send_io_live_repair for a read below extent limit,
        // at extent limit, and above extent limit.

        // Below limit
        let request = ReadRequest {
            eid: 0,
            offset: Block::new_512(7),
        };
        let op = IOop::Read {
            dependencies: vec![],
            requests: vec![request],
        };
        assert!(op.send_io_live_repair(Some(2)));

        // At limit
        let request = ReadRequest {
            eid: 2,
            offset: Block::new_512(7),
        };
        let op = IOop::Read {
            dependencies: vec![],
            requests: vec![request],
        };
        assert!(op.send_io_live_repair(Some(2)));

        let request = ReadRequest {
            eid: 3,
            offset: Block::new_512(7),
        };
        let op = IOop::Read {
            dependencies: vec![],
            requests: vec![request],
        };
        // We are past the extent limit, so this should return false
        assert!(!op.send_io_live_repair(Some(2)));

        // If we change the extent limit, it should become true
        assert!(op.send_io_live_repair(Some(3)));
    }

    // Construct an IOop::Write or IOop::WriteUnwritten at the given extent
    fn write_at_extent(eid: u64, wu: bool) -> IOop {
        let request = crucible_protocol::Write {
            eid,
            offset: Block::new_512(7),
            data: Bytes::from(vec![1]),
            block_context: BlockContext {
                encryption_context: None,
                hash: 0,
            },
        };

        let writes = vec![request];

        if wu {
            IOop::WriteUnwritten {
                dependencies: vec![],
                writes,
            }
        } else {
            IOop::Write {
                dependencies: vec![],
                writes,
            }
        }
    }

    #[tokio::test]
    async fn send_io_live_repair_write() {
        // Check the send_io_live_repair for a write below extent limit,
        // at extent limit, and above extent limit.

        // Below limit
        let wr = write_at_extent(0, false);
        assert!(wr.send_io_live_repair(Some(2)));

        // At the limit
        let wr = write_at_extent(2, false);
        assert!(wr.send_io_live_repair(Some(2)));

        // Above the limit
        let wr = write_at_extent(3, false);
        assert!(!wr.send_io_live_repair(Some(2)));

        // Back to being below the limit
        assert!(wr.send_io_live_repair(Some(3)));
    }

    #[tokio::test]
    async fn send_io_live_repair_unwritten_write() {
        // Check the send_io_live_repair for a write unwritten below extent
        // at extent limit, and above extent limit.

        // Below limit
        let wr = write_at_extent(0, true);
        assert!(wr.send_io_live_repair(Some(2)));

        // At the limit
        let wr = write_at_extent(2, true);
        assert!(wr.send_io_live_repair(Some(2)));

        // Above the limit
        let wr = write_at_extent(3, true);
        assert!(!wr.send_io_live_repair(Some(2)));

        // Back to being below the limit
        assert!(wr.send_io_live_repair(Some(3)));
    }

    #[tokio::test]
    async fn write_after_write_fail_is_alright() {
        // Verify that if a single write fails on a downstairs, a second
        // write can still be acked.
        // Then, send a flush and verify the work queue is cleared.
        let up = Upstairs::test_default(None);
        let (ds_done_tx, _ds_done_rx) = mpsc::channel(500);
        for cid in ClientId::iter() {
            up.force_ds_state(cid, DsState::WaitActive);
            up.force_ds_state(cid, DsState::WaitQuorum);
            up.force_ds_state(cid, DsState::Active);
        }
        up.force_active().unwrap();

        // Create the write that fails on one DS
        let next_id = enqueue_write(&up, true, ds_done_tx.clone()).await;

        // Make the error and ok responses
        let err_response = Err(CrucibleError::GenericError("bad".to_string()));
        let ok_response = Ok(vec![]);

        // Process the operation for client 0
        assert!(!up
            .process_ds_operation(
                next_id,
                ClientId::new(0),
                ok_response.clone(),
                None
            )
            .await
            .unwrap(),);

        // Process the error for client 1
        assert!(!up
            .process_ds_operation(next_id, ClientId::new(1), err_response, None)
            .await
            .unwrap());

        // process_ds_operation should return true after we process this.
        assert!(up
            .process_ds_operation(
                next_id,
                ClientId::new(2),
                ok_response.clone(),
                None
            )
            .await
            .unwrap(),);

        // Verify client states
        assert_eq!(up.ds_state(ClientId::new(0)).await, DsState::Active);
        assert_eq!(up.ds_state(ClientId::new(1)).await, DsState::Faulted);
        assert_eq!(up.ds_state(ClientId::new(2)).await, DsState::Active);

        // A faulted write won't change skipped job count.
        let ds = &up.downstairs;
        assert_eq!(ds.clients[ClientId::new(0)].skipped_jobs.len(), 0);
        assert_eq!(ds.clients[ClientId::new(1)].skipped_jobs.len(), 0);
        assert_eq!(ds.clients[ClientId::new(2)].skipped_jobs.len(), 0);
        drop(ds);

        // Verify we can ack this work
        assert_eq!(up.downstairs.lock().await.ackable_work().len(), 1);

        let first_id = next_id;
        // Now, do another write.
        let next_id = enqueue_write(&up, false, ds_done_tx.clone()).await;
        let ds = &mut up.downstairs;
        ds.in_progress(next_id, ClientId::new(0));
        assert_eq!(ds.in_progress(next_id, ClientId::new(1)), None);
        ds.in_progress(next_id, ClientId::new(2));
        drop(ds);

        // Process the operation for client 0, re-use ok_response from above.
        // This will return false as we don't have enough work done yet.
        assert!(!up
            .process_ds_operation(
                next_id,
                ClientId::new(0),
                ok_response.clone(),
                None
            )
            .await
            .unwrap());

        // We don't process client 1, it had failed

        // process_ds_operation should return true after we process this.
        assert!(up
            .process_ds_operation(next_id, ClientId::new(2), ok_response, None)
            .await
            .unwrap());

        // Verify we can ack this work, the total is now 2 jobs to ack
        assert_eq!(up.downstairs.lock().await.ackable_work().len(), 2);

        // One downstairs should have a skipped job on its list.
        let ds = &up.downstairs;
        assert_eq!(ds.clients[ClientId::new(0)].skipped_jobs.len(), 0);
        assert_eq!(ds.clients[ClientId::new(1)].skipped_jobs.len(), 1);
        assert!(ds.clients[ClientId::new(1)]
            .skipped_jobs
            .contains(&JobId(1001)));
        assert_eq!(ds.clients[ClientId::new(2)].skipped_jobs.len(), 0);
        drop(ds);

        // Enqueue the flush.
        let flush_id = {
            let ds = &mut up.downstairs;

            let next_id = ds.next_id();
            let dep = ds.ds_active.deps_for_flush(next_id);
            let op = create_flush(next_id, dep, 10, 0, 0, None, None);
            ds.enqueue(op, ds_done_tx.clone()).await;

            assert!(ds.in_progress(next_id, ClientId::new(0)).is_some());
            // As this DS is failed, it should return none
            assert_eq!(ds.in_progress(next_id, ClientId::new(1)), None);
            assert!(ds.in_progress(next_id, ClientId::new(2)).is_some());

            next_id
        };

        let ok_response = Ok(vec![]);
        // Process the operation for client 0
        assert!(!up
            .process_ds_operation(
                flush_id,
                ClientId::new(0),
                ok_response.clone(),
                None
            )
            .await
            .unwrap());

        // process_ds_operation should return true after we process client 2.
        assert!(up
            .process_ds_operation(flush_id, ClientId::new(2), ok_response, None)
            .await
            .unwrap());

        // ACK all the jobs and let retire_check move things along.
        let ds = &mut up.downstairs;
        assert_eq!(ds.ackable_work().len(), 3);
        ds.ack(first_id);
        ds.ack(next_id);
        ds.ack(flush_id);
        ds.retire_check(flush_id);

        assert_eq!(ds.ackable_work().len(), 0);

        // The two writes and the flush should be completed.
        assert_eq!(ds.completed().len(), 3);

        // Only the skipped flush should remain.
        assert_eq!(ds.clients[ClientId::new(0)].skipped_jobs.len(), 0);
        assert_eq!(ds.clients[ClientId::new(1)].skipped_jobs.len(), 1);
        assert!(ds.clients[ClientId::new(1)]
            .skipped_jobs
            .contains(&JobId(1002)));
        assert_eq!(ds.clients[ClientId::new(2)].skipped_jobs.len(), 0);
    }

    #[tokio::test]
    async fn write_fail_skips_new_jobs() {
        // Verify that if a single write fails on a downstairs, any
        // work that was IOState::New for that downstairs will change
        // to IOState::Skipped.  This also verifies that the list of
        // skipped jobs for each downstairs has the inflight job added
        // to it.
        let up = Upstairs::test_default(None);
        let (ds_done_tx, _ds_done_rx) = mpsc::channel(500);
        for cid in ClientId::iter() {
            up.force_ds_state(cid, DsState::WaitActive);
            up.force_ds_state(cid, DsState::WaitQuorum);
            up.force_ds_state(cid, DsState::Active);
        }
        up.force_active().unwrap();

        // Create the write that fails on one DS
        let write_id = enqueue_write(&up, true, ds_done_tx.clone()).await;

        // Now, add a read.  Don't move it to InProgress yet.
        let (request, iblocks) = generic_read_request();
        let read_id = enqueue_read(
            &up,
            request.clone(),
            iblocks,
            false,
            ds_done_tx.clone(),
        )
        .await;

        // Verify the read is all new still
        let ds = &up.downstairs;
        let job = ds.ds_active.get(&read_id).unwrap();

        assert_eq!(job.state[ClientId::new(0)], IOState::New);
        assert_eq!(job.state[ClientId::new(1)], IOState::New);
        assert_eq!(job.state[ClientId::new(2)], IOState::New);

        drop(ds);

        // Make the error and ok responses
        let err_response = Err(CrucibleError::GenericError("bad".to_string()));

        // Process the operation for client 0
        assert!(!up
            .process_ds_operation(write_id, ClientId::new(0), Ok(vec![]), None)
            .await
            .unwrap());

        // Process the error for client 1
        assert!(!up
            .process_ds_operation(
                write_id,
                ClientId::new(1),
                err_response,
                None
            )
            .await
            .unwrap());

        // process_ds_operation should return true after we process this.
        assert!(up
            .process_ds_operation(write_id, ClientId::new(2), Ok(vec![]), None)
            .await
            .unwrap(),);

        // Verify client states
        assert_eq!(up.ds_state(ClientId::new(0)).await, DsState::Active);
        assert_eq!(up.ds_state(ClientId::new(1)).await, DsState::Faulted);
        assert_eq!(up.ds_state(ClientId::new(2)).await, DsState::Active);

        // Verify we can ack this work
        assert_eq!(up.downstairs.lock().await.ackable_work().len(), 1);

        // Verify the read switched from new to skipped
        let ds = &up.downstairs;
        let job = ds.ds_active.get(&read_id).unwrap();

        assert_eq!(job.state[ClientId::new(0)], IOState::New);
        assert_eq!(job.state[ClientId::new(1)], IOState::Skipped);
        assert_eq!(job.state[ClientId::new(2)], IOState::New);

        assert_eq!(ds.clients[ClientId::new(0)].skipped_jobs.len(), 0);
        assert_eq!(ds.clients[ClientId::new(1)].skipped_jobs.len(), 1);
        assert_eq!(ds.clients[ClientId::new(2)].skipped_jobs.len(), 0);

        drop(ds);
    }

    #[tokio::test]
    async fn write_fail_skips_inprogress_jobs() {
        // Verify that if a single write fails on a downstairs, any
        // work that was IOState::InProgress for that downstairs will change
        // to IOState::Skipped.
        let up = Upstairs::test_default(None);
        let (ds_done_tx, _ds_done_rx) = mpsc::channel(500);
        for cid in ClientId::iter() {
            up.force_ds_state(cid, DsState::WaitActive);
            up.force_ds_state(cid, DsState::WaitQuorum);
            up.force_ds_state(cid, DsState::Active);
        }
        up.force_active().unwrap();

        // Create the write that fails on one DS
        let write_id = enqueue_write(&up, true, ds_done_tx.clone()).await;

        // Now, add a read.
        let (request, iblocks) = generic_read_request();
        let read_id = enqueue_read(
            &up,
            request.clone(),
            iblocks,
            true,
            ds_done_tx.clone(),
        )
        .await;

        // Make the error and ok responses
        let err_response = Err(CrucibleError::GenericError("bad".to_string()));
        let ok_res = Ok(vec![]);

        // Process the operation for client 0
        up.process_ds_operation(
            write_id,
            ClientId::new(0),
            ok_res.clone(),
            None,
        )
        .await
        .unwrap();

        // Process the error for client 1
        up.process_ds_operation(write_id, ClientId::new(1), err_response, None)
            .await
            .unwrap();

        // process_ds_operation should return true after we process this.
        assert!(up
            .process_ds_operation(write_id, ClientId::new(2), ok_res, None)
            .await
            .unwrap());

        // Verify client states
        assert_eq!(up.ds_state(ClientId::new(0)).await, DsState::Active);
        assert_eq!(up.ds_state(ClientId::new(1)).await, DsState::Faulted);
        assert_eq!(up.ds_state(ClientId::new(2)).await, DsState::Active);

        // Verify we can ack this work
        assert_eq!(up.downstairs.lock().await.ackable_work().len(), 1);

        // Verify the read switched from new to skipped
        let ds = &up.downstairs;
        let job = ds.ds_active.get(&read_id).unwrap();

        assert_eq!(job.state[ClientId::new(0)], IOState::InProgress);
        assert_eq!(job.state[ClientId::new(1)], IOState::Skipped);
        assert_eq!(job.state[ClientId::new(2)], IOState::InProgress);

        assert_eq!(ds.clients[ClientId::new(0)].skipped_jobs.len(), 0);
        assert_eq!(ds.clients[ClientId::new(1)].skipped_jobs.len(), 1);
        assert_eq!(ds.clients[ClientId::new(2)].skipped_jobs.len(), 0);
        drop(ds);
    }

    #[tokio::test]
    async fn write_fail_skips_many_jobs() {
        // Create a bunch of jobs, do some, then encounter a write error.
        // Make sure that older jobs are still okay, and failed job was
        // skipped.
        let up = Upstairs::test_default(None);
        let (ds_done_tx, _ds_done_rx) = mpsc::channel(500);
        for cid in ClientId::iter() {
            up.force_ds_state(cid, DsState::WaitActive);
            up.force_ds_state(cid, DsState::WaitQuorum);
            up.force_ds_state(cid, DsState::Active);
        }
        up.force_active().unwrap();
        let write_one = enqueue_write(&up, true, ds_done_tx.clone()).await;

        // Now, add a read.
        let (request, iblocks) = generic_read_request();
        let read_one = enqueue_read(
            &up,
            request.clone(),
            iblocks,
            true,
            ds_done_tx.clone(),
        )
        .await;

        // Make the read ok response
        let rr = Ok(vec![ReadResponse::from_request_with_data(&request, &[])]);

        for cid in ClientId::iter() {
            up.process_ds_operation(write_one, cid, Ok(vec![]), None)
                .await
                .unwrap();
            up.process_ds_operation(read_one, cid, rr.clone(), None)
                .await
                .unwrap();
        }

        // Verify two jobs can be acked.
        assert_eq!(up.downstairs.lock().await.ackable_work().len(), 2);

        // Verify all IOs are done
        let ds = &up.downstairs;
        for cid in ClientId::iter() {
            let job = ds.ds_active.get(&read_one).unwrap();
            assert_eq!(job.state[cid], IOState::Done);
            let job = ds.ds_active.get(&write_one).unwrap();
            assert_eq!(job.state[cid], IOState::Done);
        }
        assert_eq!(ds.clients[ClientId::new(0)].skipped_jobs.len(), 0);
        assert_eq!(ds.clients[ClientId::new(1)].skipped_jobs.len(), 0);
        assert_eq!(ds.clients[ClientId::new(2)].skipped_jobs.len(), 0);
        drop(ds);

        // New write, this one will have a failure
        // Create a write
        let write_fail = enqueue_write(&up, true, ds_done_tx.clone()).await;

        let err_response = Err(CrucibleError::GenericError("bad".to_string()));

        // Process the operation for client 0, 1
        up.process_ds_operation(write_fail, ClientId::new(0), Ok(vec![]), None)
            .await
            .unwrap();
        up.process_ds_operation(write_fail, ClientId::new(1), Ok(vec![]), None)
            .await
            .unwrap();
        up.process_ds_operation(
            write_fail,
            ClientId::new(2),
            err_response,
            None,
        )
        .await
        .unwrap();

        // Verify client states
        assert_eq!(up.ds_state(ClientId::new(0)).await, DsState::Active);
        assert_eq!(up.ds_state(ClientId::new(1)).await, DsState::Active);
        assert_eq!(up.ds_state(ClientId::new(2)).await, DsState::Faulted);

        // Verify we can ack this work plus the previous two
        assert_eq!(up.downstairs.lock().await.ackable_work().len(), 3);

        // Verify all IOs are done
        let ds = &up.downstairs;
        for cid in ClientId::iter() {
            let job = ds.ds_active.get(&read_one).unwrap();
            assert_eq!(job.state[cid], IOState::Done);
            let job = ds.ds_active.get(&write_one).unwrap();
            assert_eq!(job.state[cid], IOState::Done);
        }
        let job = ds.ds_active.get(&write_fail).unwrap();
        assert_eq!(job.state[ClientId::new(0)], IOState::Done);
        assert_eq!(job.state[ClientId::new(1)], IOState::Done);
        assert_eq!(
            job.state[ClientId::new(2)],
            IOState::Error(CrucibleError::GenericError("bad".to_string()))
        );

        // A failed job does not change the skipped count.
        assert_eq!(ds.clients[ClientId::new(0)].skipped_jobs.len(), 0);
        assert_eq!(ds.clients[ClientId::new(1)].skipped_jobs.len(), 0);
        assert_eq!(ds.clients[ClientId::new(2)].skipped_jobs.len(), 0);
        drop(ds);
    }

    #[tokio::test]
    async fn write_fail_past_present_future() {
        // Create a bunch of jobs, finish some, then encounter a write error.
        // Make sure that older jobs are still okay, failed job was error,
        // and jobs not yet started on the faulted downstairs have
        // transitioned to skipped.
        let up = Upstairs::test_default(None);
        let (ds_done_tx, _ds_done_rx) = mpsc::channel(500);
        for cid in ClientId::iter() {
            up.force_ds_state(cid, DsState::WaitActive);
            up.force_ds_state(cid, DsState::WaitQuorum);
            up.force_ds_state(cid, DsState::Active);
        }
        up.force_active().unwrap();

        // Create a write
        let write_one = enqueue_write(&up, true, ds_done_tx.clone()).await;

        // Now, add a read.
        let (request, iblocks) = generic_read_request();
        let read_one = enqueue_read(
            &up,
            request.clone(),
            iblocks,
            true,
            ds_done_tx.clone(),
        )
        .await;

        // Make the read ok response
        let rr = Ok(vec![ReadResponse::from_request_with_data(&request, &[])]);

        for cid in ClientId::iter() {
            up.process_ds_operation(write_one, cid, Ok(vec![]), None)
                .await
                .unwrap();
            up.process_ds_operation(read_one, cid, rr.clone(), None)
                .await
                .unwrap();
        }

        // Verify two jobs can be acked.
        assert_eq!(up.downstairs.lock().await.ackable_work().len(), 2);

        // Verify all IOs are done
        let ds = &up.downstairs;
        for cid in ClientId::iter() {
            let job = ds.ds_active.get(&read_one).unwrap();
            assert_eq!(job.state[cid], IOState::Done);
            let job = ds.ds_active.get(&write_one).unwrap();
            assert_eq!(job.state[cid], IOState::Done);
        }
        drop(ds);

        // Create a New write, this one will fail on one downstairs
        let write_fail = enqueue_write(&up, true, ds_done_tx.clone()).await;
        // Response for the write failure
        let err_response = Err(CrucibleError::GenericError("bad".to_string()));

        // Create some reads as well that will be InProgress
        let read_two = enqueue_read(
            &up,
            request.clone(),
            iblocks,
            true,
            ds_done_tx.clone(),
        )
        .await;

        // Process the write operation for downstairs 0, 1
        up.process_ds_operation(write_fail, ClientId::new(0), Ok(vec![]), None)
            .await
            .unwrap();
        up.process_ds_operation(write_fail, ClientId::new(1), Ok(vec![]), None)
            .await
            .unwrap();
        // Have downstairs 2 return error.
        up.process_ds_operation(
            write_fail,
            ClientId::new(2),
            err_response,
            None,
        )
        .await
        .unwrap();

        // Verify client states
        assert_eq!(up.ds_state(ClientId::new(0)).await, DsState::Active);
        assert_eq!(up.ds_state(ClientId::new(1)).await, DsState::Active);
        assert_eq!(up.ds_state(ClientId::new(2)).await, DsState::Faulted);

        // Verify we can ack this work plus the previous two
        assert_eq!(up.downstairs.lock().await.ackable_work().len(), 3);

        // Verify all IOs are done
        let ds = &up.downstairs;
        for cid in ClientId::iter() {
            // First read, still Done
            let job = ds.ds_active.get(&read_one).unwrap();
            assert_eq!(job.state[cid], IOState::Done);
            // First write, still Done
            let job = ds.ds_active.get(&write_one).unwrap();
            assert_eq!(job.state[cid], IOState::Done);
        }
        // The failing write, done on 0,1
        let job = ds.ds_active.get(&write_fail).unwrap();
        assert_eq!(job.state[ClientId::new(0)], IOState::Done);
        assert_eq!(job.state[ClientId::new(1)], IOState::Done);
        // The failing write, error on 2
        assert_eq!(
            job.state[ClientId::new(2)],
            IOState::Error(CrucibleError::GenericError("bad".to_string()))
        );

        // The reads that were in progress
        let job = ds.ds_active.get(&read_two).unwrap();
        assert_eq!(job.state[ClientId::new(0)], IOState::InProgress);
        assert_eq!(job.state[ClientId::new(1)], IOState::InProgress);
        assert_eq!(job.state[ClientId::new(2)], IOState::Skipped);

        assert_eq!(ds.clients[ClientId::new(0)].skipped_jobs.len(), 0);
        assert_eq!(ds.clients[ClientId::new(1)].skipped_jobs.len(), 0);
        assert_eq!(ds.clients[ClientId::new(2)].skipped_jobs.len(), 1);
        drop(ds);
    }

    #[tokio::test]
    async fn faulted_downstairs_skips_work() {
        // Verify that any job submitted with a faulted downstairs is
        // automatically moved to skipped.
        let up = Upstairs::test_default(None);
        let (ds_done_tx, _ds_done_rx) = mpsc::channel(500);
        for cid in ClientId::iter() {
            up.force_ds_state(cid, DsState::WaitActive);
            up.force_ds_state(cid, DsState::WaitQuorum);
            up.force_ds_state(cid, DsState::Active);
        }
        up.force_active().unwrap();
        up.force_ds_state(ClientId::new(0), DsState::Faulted);

        // Create a write
        let write_one = enqueue_write(&up, false, ds_done_tx.clone()).await;

        // Now, add a read.
        let (request, iblocks) = generic_read_request();

        let read_one = enqueue_read(
            &up,
            request.clone(),
            iblocks,
            false,
            ds_done_tx.clone(),
        )
        .await;

        let flush_one = enqueue_flush(&up, false, ds_done_tx.clone()).await;

        let ds = &up.downstairs;
        let job = ds.ds_active.get(&write_one).unwrap();
        assert_eq!(job.state[ClientId::new(0)], IOState::Skipped);
        assert_eq!(job.state[ClientId::new(1)], IOState::New);
        assert_eq!(job.state[ClientId::new(2)], IOState::New);

        let job = ds.ds_active.get(&read_one).unwrap();
        assert_eq!(job.state[ClientId::new(0)], IOState::Skipped);
        assert_eq!(job.state[ClientId::new(1)], IOState::New);
        assert_eq!(job.state[ClientId::new(2)], IOState::New);

        let job = ds.ds_active.get(&flush_one).unwrap();
        assert_eq!(job.state[ClientId::new(0)], IOState::Skipped);
        assert_eq!(job.state[ClientId::new(1)], IOState::New);
        assert_eq!(job.state[ClientId::new(2)], IOState::New);

        // Three skipped jobs for downstairs zero
        assert_eq!(ds.clients[ClientId::new(0)].skipped_jobs.len(), 3);
        assert_eq!(ds.clients[ClientId::new(1)].skipped_jobs.len(), 0);
        assert_eq!(ds.clients[ClientId::new(2)].skipped_jobs.len(), 0);
        drop(ds);
    }

    #[tokio::test]
    async fn faulted_downstairs_skips_but_still_does_work() {
        // Verify work can progress through the work queue even when one
        // downstairs has failed. One write, one read, and one flush.
        let up = Upstairs::test_default(None);
        let (ds_done_tx, _ds_done_rx) = mpsc::channel(500);
        for cid in ClientId::iter() {
            up.force_ds_state(cid, DsState::WaitActive);
            up.force_ds_state(cid, DsState::WaitQuorum);
            up.force_ds_state(cid, DsState::Active);
        }
        up.force_active().unwrap();
        up.force_ds_state(ClientId::new(0), DsState::Faulted);

        // Create a write
        let write_one = enqueue_write(&up, true, ds_done_tx.clone()).await;

        // Now, add a read.
        let (request, iblocks) = generic_read_request();

        let read_one = enqueue_read(
            &up,
            request.clone(),
            iblocks,
            true,
            ds_done_tx.clone(),
        )
        .await;

        // Finally, add a flush
        let flush_one = enqueue_flush(&up, true, ds_done_tx.clone()).await;

        let ds = &up.downstairs;
        let job = ds.ds_active.get(&write_one).unwrap();
        assert_eq!(job.state[ClientId::new(0)], IOState::Skipped);
        assert_eq!(job.state[ClientId::new(1)], IOState::InProgress);
        assert_eq!(job.state[ClientId::new(2)], IOState::InProgress);

        let job = ds.ds_active.get(&read_one).unwrap();
        assert_eq!(job.state[ClientId::new(0)], IOState::Skipped);
        assert_eq!(job.state[ClientId::new(1)], IOState::InProgress);
        assert_eq!(job.state[ClientId::new(2)], IOState::InProgress);

        let job = ds.ds_active.get(&flush_one).unwrap();
        assert_eq!(job.state[ClientId::new(0)], IOState::Skipped);
        assert_eq!(job.state[ClientId::new(1)], IOState::InProgress);
        assert_eq!(job.state[ClientId::new(2)], IOState::InProgress);

        // Three skipped jobs on downstairs client 0
        assert_eq!(ds.clients[ClientId::new(0)].skipped_jobs.len(), 3);
        assert_eq!(ds.clients[ClientId::new(1)].skipped_jobs.len(), 0);
        assert_eq!(ds.clients[ClientId::new(2)].skipped_jobs.len(), 0);
        drop(ds);

        // Do the write
        up.process_ds_operation(write_one, ClientId::new(1), Ok(vec![]), None)
            .await
            .unwrap();
        up.process_ds_operation(write_one, ClientId::new(2), Ok(vec![]), None)
            .await
            .unwrap();

        // Make the read ok response, do the read
        let rr = Ok(vec![ReadResponse::from_request_with_data(&request, &[])]);

        up.process_ds_operation(read_one, ClientId::new(1), rr.clone(), None)
            .await
            .unwrap();
        up.process_ds_operation(read_one, ClientId::new(2), rr.clone(), None)
            .await
            .unwrap();

        // Do the flush
        up.process_ds_operation(flush_one, ClientId::new(1), Ok(vec![]), None)
            .await
            .unwrap();
        up.process_ds_operation(flush_one, ClientId::new(2), Ok(vec![]), None)
            .await
            .unwrap();

        // Verify three jobs can be acked.
        assert_eq!(up.downstairs.lock().await.ackable_work().len(), 3);

        // Verify all IOs are done
        let ds = &mut up.downstairs;

        let job = ds.ds_active.get(&read_one).unwrap();
        assert_eq!(job.state[ClientId::new(1)], IOState::Done);
        assert_eq!(job.state[ClientId::new(2)], IOState::Done);
        let job = ds.ds_active.get(&write_one).unwrap();
        assert_eq!(job.state[ClientId::new(1)], IOState::Done);
        assert_eq!(job.state[ClientId::new(2)], IOState::Done);
        let job = ds.ds_active.get(&flush_one).unwrap();
        assert_eq!(job.state[ClientId::new(1)], IOState::Done);
        assert_eq!(job.state[ClientId::new(2)], IOState::Done);

        ds.ack(read_one);
        ds.ack(write_one);
        ds.ack(flush_one);
        ds.retire_check(flush_one);

        // Skipped jobs just has the flush
        assert_eq!(ds.clients[ClientId::new(0)].skipped_jobs.len(), 1);
        assert!(ds.clients[ClientId::new(0)]
            .skipped_jobs
            .contains(&JobId(1002)));
        assert_eq!(ds.ackable_work().len(), 0);

        // The writes, the read, and the flush should be completed.
        assert_eq!(ds.completed().len(), 3);
        // No more ackable work
        assert_eq!(ds.ackable_work().len(), 0);
        // No more jobs on the queue
        assert_eq!(ds.active_count(), 0);
    }

    #[tokio::test]
    async fn two_faulted_downstairs_can_still_read() {
        // Verify we can still read (and clear the work queue) with only
        // one downstairs.
        let up = Upstairs::test_default(None);
        let (ds_done_tx, _ds_done_rx) = mpsc::channel(500);

        for cid in ClientId::iter() {
            up.force_ds_state(cid, DsState::WaitActive);
            up.force_ds_state(cid, DsState::WaitQuorum);
            up.force_ds_state(cid, DsState::Active);
        }
        up.force_active().unwrap();
        up.force_ds_state(ClientId::new(0), DsState::Faulted);
        up.force_ds_state(ClientId::new(2), DsState::Faulted);

        // Create a write
        let write_one = enqueue_write(&up, true, ds_done_tx.clone()).await;

        // Now, add a read.
        let (request, iblocks) = generic_read_request();

        let read_one = enqueue_read(
            &up,
            request.clone(),
            iblocks,
            true,
            ds_done_tx.clone(),
        )
        .await;

        // Finally, add a flush
        let flush_one = enqueue_flush(&up, true, ds_done_tx.clone()).await;

        let ds = &up.downstairs;
        let job = ds.ds_active.get(&write_one).unwrap();
        assert_eq!(job.state[ClientId::new(0)], IOState::Skipped);
        assert_eq!(job.state[ClientId::new(1)], IOState::InProgress);
        assert_eq!(job.state[ClientId::new(2)], IOState::Skipped);

        let job = ds.ds_active.get(&read_one).unwrap();
        assert_eq!(job.state[ClientId::new(0)], IOState::Skipped);
        assert_eq!(job.state[ClientId::new(1)], IOState::InProgress);
        assert_eq!(job.state[ClientId::new(2)], IOState::Skipped);

        let job = ds.ds_active.get(&flush_one).unwrap();
        assert_eq!(job.state[ClientId::new(0)], IOState::Skipped);
        assert_eq!(job.state[ClientId::new(1)], IOState::InProgress);
        assert_eq!(job.state[ClientId::new(2)], IOState::Skipped);

        // Skipped jobs added on downstairs client 0
        assert_eq!(ds.clients[ClientId::new(0)].skipped_jobs.len(), 3);
        assert_eq!(ds.clients[ClientId::new(1)].skipped_jobs.len(), 0);
        assert_eq!(ds.clients[ClientId::new(2)].skipped_jobs.len(), 3);
        drop(ds);

        // Do the write
        up.process_ds_operation(write_one, ClientId::new(1), Ok(vec![]), None)
            .await
            .unwrap();

        // Make the read ok response, do the read
        let rr = Ok(vec![ReadResponse::from_request_with_data(&request, &[])]);

        up.process_ds_operation(read_one, ClientId::new(1), rr.clone(), None)
            .await
            .unwrap();

        // Do the flush
        up.process_ds_operation(flush_one, ClientId::new(1), Ok(vec![]), None)
            .await
            .unwrap();

        // Verify three jobs can be acked.
        assert_eq!(up.downstairs.lock().await.ackable_work().len(), 3);

        // Verify all IOs are done
        let ds = &mut up.downstairs;

        let job = ds.ds_active.get(&read_one).unwrap();
        assert_eq!(job.state[ClientId::new(1)], IOState::Done);
        let job = ds.ds_active.get(&write_one).unwrap();
        assert_eq!(job.state[ClientId::new(1)], IOState::Done);
        let job = ds.ds_active.get(&flush_one).unwrap();
        assert_eq!(job.state[ClientId::new(1)], IOState::Done);

        ds.ack(read_one);
        ds.ack(write_one);
        ds.ack(flush_one);
        ds.retire_check(flush_one);

        // Skipped jobs now just have the flush.
        assert_eq!(ds.clients[ClientId::new(0)].skipped_jobs.len(), 1);
        assert!(ds.clients[ClientId::new(0)]
            .skipped_jobs
            .contains(&JobId(1002)));
        assert_eq!(ds.clients[ClientId::new(2)].skipped_jobs.len(), 1);
        assert!(ds.clients[ClientId::new(2)]
            .skipped_jobs
            .contains(&JobId(1002)));
        assert_eq!(ds.ackable_work().len(), 0);

        // The writes, the read, and the flush should be completed.
        assert_eq!(ds.completed().len(), 3);
        // No more ackable work
        assert_eq!(ds.ackable_work().len(), 0);
        // No more jobs on the queue
        assert_eq!(ds.active_count(), 0);
    }

    #[tokio::test]
    async fn three_faulted_enqueue_will_handle_read() {
        // When three downstairs are faulted, verify that enqueue will move
        // work through the queue for us.
        let up = Upstairs::test_default(None);
        let (ds_done_tx, _ds_done_rx) = mpsc::channel(500);

        for cid in ClientId::iter() {
            up.force_ds_state(cid, DsState::WaitActive);
            up.force_ds_state(cid, DsState::WaitQuorum);
            up.force_ds_state(cid, DsState::Active);
        }
        up.force_active().unwrap();
        for cid in ClientId::iter() {
            up.force_ds_state(cid, DsState::Faulted);
        }

        // Create a read.
        let (request, iblocks) = generic_read_request();

        let read_one = enqueue_read(
            &up,
            request.clone(),
            iblocks,
            false,
            ds_done_tx.clone(),
        )
        .await;

        let ds = &up.downstairs;
        let job = ds.ds_active.get(&read_one).unwrap();
        assert_eq!(job.state[ClientId::new(0)], IOState::Skipped);
        assert_eq!(job.state[ClientId::new(1)], IOState::Skipped);
        assert_eq!(job.state[ClientId::new(2)], IOState::Skipped);

        assert_eq!(ds.clients[ClientId::new(0)].skipped_jobs.len(), 1);
        assert_eq!(ds.clients[ClientId::new(1)].skipped_jobs.len(), 1);
        assert_eq!(ds.clients[ClientId::new(2)].skipped_jobs.len(), 1);
        drop(ds);

        // Verify jobs can be acked.
        assert_eq!(up.downstairs.lock().await.ackable_work().len(), 1);

        // Verify all IOs are done
        // We are simulating what would happen here by the up_ds_listen
        // task, after it receives a notification from the ds_done_tx.
        let ds = &mut up.downstairs;
        ds.ack(read_one);

        ds.retire_check(read_one);

        // Our skipped jobs have not yet been cleared.
        assert_eq!(ds.clients[ClientId::new(0)].skipped_jobs.len(), 1);
        assert_eq!(ds.clients[ClientId::new(1)].skipped_jobs.len(), 1);
        assert_eq!(ds.clients[ClientId::new(2)].skipped_jobs.len(), 1);
        assert_eq!(ds.ackable_work().len(), 0);
    }

    #[tokio::test]
    async fn three_faulted_enqueue_will_handle_write() {
        // When three downstairs are faulted, verify that enqueue will move
        // work through the queue for us.
        let up = Upstairs::test_default(None);
        let (ds_done_tx, _ds_done_rx) = mpsc::channel(500);

        for cid in ClientId::iter() {
            up.force_ds_state(cid, DsState::WaitActive);
            up.force_ds_state(cid, DsState::WaitQuorum);
            up.force_ds_state(cid, DsState::Active);
        }
        up.force_active().unwrap();
        for cid in ClientId::iter() {
            up.force_ds_state(cid, DsState::Faulted);
        }

        // Create a write.
        let write_one = enqueue_write(&up, true, ds_done_tx.clone()).await;

        let ds = &up.downstairs;
        let job = ds.ds_active.get(&write_one).unwrap();
        assert_eq!(job.state[ClientId::new(0)], IOState::Skipped);
        assert_eq!(job.state[ClientId::new(1)], IOState::Skipped);
        assert_eq!(job.state[ClientId::new(2)], IOState::Skipped);

        assert_eq!(ds.clients[ClientId::new(0)].skipped_jobs.len(), 1);
        assert_eq!(ds.clients[ClientId::new(1)].skipped_jobs.len(), 1);
        assert_eq!(ds.clients[ClientId::new(2)].skipped_jobs.len(), 1);
        drop(ds);

        // Verify jobs can be acked.
        assert_eq!(up.downstairs.lock().await.ackable_work().len(), 1);

        // Verify all IOs are done
        // We are simulating what would happen here by the up_ds_listen
        // task, after it receives a notification from the ds_done_tx.
        let ds = &mut up.downstairs;
        ds.ack(write_one);

        ds.retire_check(write_one);
        // No flush, no change in skipped jobs.
        assert_eq!(ds.clients[ClientId::new(0)].skipped_jobs.len(), 1);
        assert_eq!(ds.clients[ClientId::new(1)].skipped_jobs.len(), 1);
        assert_eq!(ds.clients[ClientId::new(2)].skipped_jobs.len(), 1);

        assert_eq!(ds.ackable_work().len(), 0);
    }

    #[tokio::test]
    async fn three_faulted_enqueue_will_handle_flush() {
        // When three downstairs are faulted, verify that enqueue will move
        // work through the queue for us.
        let up = Upstairs::test_default(None);
        let (ds_done_tx, _ds_done_rx) = mpsc::channel(500);

        for cid in ClientId::iter() {
            up.force_ds_state(cid, DsState::WaitActive);
            up.force_ds_state(cid, DsState::WaitQuorum);
            up.force_ds_state(cid, DsState::Active);
        }
        up.force_active().unwrap();
        for cid in ClientId::iter() {
            up.force_ds_state(cid, DsState::Faulted);
        }

        // Create a flush.
        let flush_one = enqueue_flush(&up, false, ds_done_tx.clone()).await;

        let ds = &up.downstairs;
        let job = ds.ds_active.get(&flush_one).unwrap();
        assert_eq!(job.state[ClientId::new(0)], IOState::Skipped);
        assert_eq!(job.state[ClientId::new(1)], IOState::Skipped);
        assert_eq!(job.state[ClientId::new(2)], IOState::Skipped);
        for cid in ClientId::iter() {
            assert_eq!(ds.clients[cid].skipped_jobs.len(), 1);
            assert!(ds.clients[cid].skipped_jobs.contains(&JobId(1000)));
        }
        drop(ds);

        // Verify jobs can be acked.
        assert_eq!(up.downstairs.lock().await.ackable_work().len(), 1);

        let ds = &mut up.downstairs;
        ds.ack(flush_one);

        ds.retire_check(flush_one);

        assert_eq!(ds.ackable_work().len(), 0);

        // The flush should remove all work from the ds queue.
        assert_eq!(ds.completed().len(), 1);
        // No more ackable work
        assert_eq!(ds.ackable_work().len(), 0);
        // No more jobs on the queue
        assert_eq!(ds.active_count(), 0);

        // Skipped jobs still has the flush.
        for cid in ClientId::iter() {
            assert_eq!(ds.clients[cid].skipped_jobs.len(), 1);
            assert!(ds.clients[cid].skipped_jobs.contains(&JobId(1000)));
        }
        drop(ds);
    }

    #[tokio::test]
    async fn three_faulted_enqueue_will_handle_many_ios() {
        // When three downstairs are faulted, verify that enqueue will move
        // work through the queue for us. Several jobs are submitted and
        // a final flush should clean them out.
        let up = Upstairs::test_default(None);
        let (ds_done_tx, _ds_done_rx) = mpsc::channel(500);

        for cid in ClientId::iter() {
            up.force_ds_state(cid, DsState::WaitActive);
            up.force_ds_state(cid, DsState::WaitQuorum);
            up.force_ds_state(cid, DsState::Active);
        }
        up.force_active().unwrap();
        for cid in ClientId::iter() {
            up.force_ds_state(cid, DsState::Faulted);
        }

        // Create a read.
        let (request, iblocks) = generic_read_request();

        let read_one = enqueue_read(
            &up,
            request.clone(),
            iblocks,
            false,
            ds_done_tx.clone(),
        )
        .await;
        // Create a write.
        let write_one = enqueue_write(&up, true, ds_done_tx.clone()).await;
        let flush_one = enqueue_flush(&up, false, ds_done_tx.clone()).await;

        let ds = &mut up.downstairs;

        // Verify all jobs can be acked.
        assert_eq!(ds.ackable_work().len(), 3);

        // Skipped jobs are not yet cleared.
        for cid in ClientId::iter() {
            assert!(ds.clients[cid].skipped_jobs.contains(&JobId(1000)));
            assert!(ds.clients[cid].skipped_jobs.contains(&JobId(1001)));
            assert!(ds.clients[cid].skipped_jobs.contains(&JobId(1002)));
            assert_eq!(ds.clients[cid].skipped_jobs.len(), 3);
        }

        // Verify all IOs are done
        // We are simulating what would happen here by the up_ds_listen
        // task, after it receives a notification from the ds_done_tx.
        ds.ack(read_one);
        ds.ack(write_one);
        ds.ack(flush_one);

        // Don't bother with retire check for read/write, just flush
        ds.retire_check(flush_one);

        assert_eq!(ds.ackable_work().len(), 0);

        // The flush should remove all work from the ds queue.
        assert_eq!(ds.completed().len(), 3);
        // No more ackable work
        assert_eq!(ds.ackable_work().len(), 0);
        // No more jobs on the queue
        assert_eq!(ds.active_count(), 0);

        // Skipped jobs now just has the flush
        for cid in ClientId::iter() {
            assert!(ds.clients[cid].skipped_jobs.contains(&JobId(1002)));
            assert_eq!(ds.clients[cid].skipped_jobs.len(), 1);
        }
    }

    #[tokio::test]
    async fn three_faulted_retire_skipped_some_leave_some() {
        // When three downstairs are faulted, verify that enqueue will move
        // work through the queue for us. Several jobs are submitted and
        // a flush, then several more jobs. Verify the jobs after the flush
        // stay on the ds_skipped_jobs list.

        let up = Upstairs::test_default(None);
        let (ds_done_tx, _ds_done_rx) = mpsc::channel(500);

        for cid in ClientId::iter() {
            up.force_ds_state(cid, DsState::WaitActive);
            up.force_ds_state(cid, DsState::WaitQuorum);
            up.force_ds_state(cid, DsState::Active);
        }
        up.force_active().unwrap();
        for cid in ClientId::iter() {
            up.force_ds_state(cid, DsState::Faulted);
        }

        // Create a read.
        let (request, iblocks) = generic_read_request();

        let read_one = enqueue_read(
            &up,
            request.clone(),
            iblocks,
            false,
            ds_done_tx.clone(),
        )
        .await;
        let write_one = enqueue_write(&up, true, ds_done_tx.clone()).await;
        let flush_one = enqueue_flush(&up, false, ds_done_tx.clone()).await;

        // Create more IOs.
        let (request, iblocks) = generic_read_request();

        let _read_two = enqueue_read(
            &up,
            request.clone(),
            iblocks,
            false,
            ds_done_tx.clone(),
        )
        .await;
        let _write_two = enqueue_write(&up, true, ds_done_tx.clone()).await;
        let _flush_two = enqueue_flush(&up, false, ds_done_tx.clone()).await;

        let ds = &mut up.downstairs;

        // Six jobs have been skipped.
        for cid in ClientId::iter() {
            assert!(ds.clients[cid].skipped_jobs.contains(&JobId(1000)));
            assert!(ds.clients[cid].skipped_jobs.contains(&JobId(1001)));
            assert!(ds.clients[cid].skipped_jobs.contains(&JobId(1002)));
            assert!(ds.clients[cid].skipped_jobs.contains(&JobId(1003)));
            assert!(ds.clients[cid].skipped_jobs.contains(&JobId(1004)));
            assert!(ds.clients[cid].skipped_jobs.contains(&JobId(1005)));
            assert_eq!(ds.clients[cid].skipped_jobs.len(), 6);
        }

        // Ack the first 3 jobs
        ds.ack(read_one);
        ds.ack(write_one);
        ds.ack(flush_one);

        assert_eq!(ds.ackable_work().len(), 3);
        // Don't bother with retire check for read/write, just flush
        ds.retire_check(flush_one);

        // The first two skipped jobs are now cleared and the non-acked
        // jobs remain on the list, as well as the last flush.
        for cid in ClientId::iter() {
            assert!(ds.clients[cid].skipped_jobs.contains(&JobId(1002)));
            assert!(ds.clients[cid].skipped_jobs.contains(&JobId(1003)));
            assert!(ds.clients[cid].skipped_jobs.contains(&JobId(1004)));
            assert!(ds.clients[cid].skipped_jobs.contains(&JobId(1005)));
            assert_eq!(ds.clients[cid].skipped_jobs.len(), 4);
        }
    }

    // verify new work can progress with one failed downstairs.
    // two failed downstairs, what to do?  All writes will fail, all flushes
    // will fail, so, what?

    // Test that multiple GtoS downstairs jobs work
    #[tokio::test]
    async fn test_multiple_gtos_bulk_read_read() {
        let up = Upstairs::test_default(None);
        up.force_active().unwrap();
        for cid in ClientId::iter() {
            up.force_ds_state(cid, DsState::WaitActive);
            up.force_ds_state(cid, DsState::WaitQuorum);
            up.force_ds_state(cid, DsState::Active);
        }

        let mut gw = up.guest.guest_work.lock().await;

        let gw_id = 12345;

        // Create two reads
        let first_id = JobId(1010);
        let second_id = JobId(1011);

        let mut data_buffers = HashMap::new();
        data_buffers.insert(first_id, Buffer::new(512));
        data_buffers.insert(second_id, Buffer::new(512));

        let mut sub = HashSet::new();
        sub.insert(first_id);
        sub.insert(second_id);

        let guest_job = GtoS::new_bulk(sub, data_buffers.clone(), None);

        gw.active.insert(gw_id, guest_job);

        let mut first_response_data = BytesMut::with_capacity(512);
        first_response_data.resize(512, 0u8);
        thread_rng().fill(&mut first_response_data[..]);
        let first_read_response_hash =
            integrity_hash(&[&first_response_data[..]]);

        let mut second_response_data = BytesMut::with_capacity(512);
        second_response_data.resize(512, 0u8);
        thread_rng().fill(&mut second_response_data[..]);
        let second_read_response_hash =
            integrity_hash(&[&second_response_data[..]]);

        let first_response = Some(vec![ReadResponse {
            eid: 0,
            offset: Block::new_512(0),
            data: first_response_data.clone(),
            block_contexts: vec![BlockContext {
                hash: first_read_response_hash,
                encryption_context: None,
            }],
        }]);

        let second_response = Some(vec![ReadResponse {
            eid: 0,
            offset: Block::new_512(0),
            data: second_response_data.clone(),
            block_contexts: vec![BlockContext {
                hash: second_read_response_hash,
                encryption_context: None,
            }],
        }]);

        gw.gw_ds_complete(gw_id, first_id, first_response, Ok(()), &up.log)
            .await;
        assert!(!gw.completed.contains(&gw_id));

        gw.gw_ds_complete(gw_id, second_id, second_response, Ok(()), &up.log)
            .await;
        assert!(gw.completed.contains(&gw_id));

        assert_eq!(
            *data_buffers.get(&first_id).unwrap().as_vec().await,
            first_response_data.to_vec()
        );
        assert_eq!(
            *data_buffers.get(&second_id).unwrap().as_vec().await,
            second_response_data.to_vec()
        );
    }

    #[test]
    fn test_check_read_response_hashes() {
        // Regular happy path
        let job = DownstairsIO {
            ds_id: JobId(1),
            guest_id: 1,
            work: IOop::Read {
                dependencies: vec![],
                requests: vec![],
            },
            state: ClientData::new(IOState::New),
            acked: false,
            replay: false,
            data: None,
            read_response_hashes: vec![Some(123)],
        };

        assert!(job.check_read_response_hashes(&[ReadResponse {
            eid: 0,
            offset: Block::new_512(1),
            data: BytesMut::default(),
            block_contexts: vec![BlockContext {
                hash: 123,
                encryption_context: None,
            }],
        }]));

        // Mismatch!
        let job = DownstairsIO {
            ds_id: JobId(1),
            guest_id: 1,
            work: IOop::Read {
                dependencies: vec![],
                requests: vec![],
            },
            state: ClientData::new(IOState::New),
            acked: false,
            replay: false,
            data: None,
            read_response_hashes: vec![Some(456)],
        };

        assert!(!job.check_read_response_hashes(&[ReadResponse {
            eid: 0,
            offset: Block::new_512(1),
            data: BytesMut::default(),
            block_contexts: vec![BlockContext {
                hash: 123,
                encryption_context: None,
            }],
        }]));

        // Length mismatch
        let job = DownstairsIO {
            ds_id: JobId(1),
            guest_id: 1,
            work: IOop::Read {
                dependencies: vec![],
                requests: vec![],
            },
            state: ClientData::new(IOState::New),
            acked: false,
            replay: false,
            data: None,
            read_response_hashes: vec![Some(456), Some(123)],
        };

        assert!(!job.check_read_response_hashes(&[ReadResponse {
            eid: 0,
            offset: Block::new_512(1),
            data: BytesMut::default(),
            block_contexts: vec![BlockContext {
                hash: 123,
                encryption_context: None,
            }],
        }]));

        let job = DownstairsIO {
            ds_id: JobId(1),
            guest_id: 1,
            work: IOop::Read {
                dependencies: vec![],
                requests: vec![],
            },
            state: ClientData::new(IOState::New),
            acked: false,
            replay: false,
            data: None,
            read_response_hashes: vec![Some(777)],
        };

        assert!(!job.check_read_response_hashes(&[ReadResponse {
            eid: 0,
            offset: Block::new_512(1),
            data: BytesMut::default(),
            block_contexts: vec![
                BlockContext {
                    hash: 123,
                    encryption_context: None,
                },
                BlockContext {
                    hash: 456,
                    encryption_context: None,
                },
            ],
        }]));
    }
}
