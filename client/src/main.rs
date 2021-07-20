use std::sync::Arc;

use anyhow::Result;
use bytes::{BufMut, BytesMut};
use tokio::runtime::Builder;

use crucible::*;

/**
 * This is an example Crucible client.
 * Here we make use of the interfaces that Crucible exposes.
 */
fn main() -> Result<()> {
    let opt = opts()?;

    /*
     * Crucible needs a runtime as it will create several async tasks to
     * handle adding new IOs, communication with the three downstairs
     * instances, and completing IOs.
     */
    let runtime = Builder::new_multi_thread()
        .worker_threads(10)
        .thread_name("crucible-tokio")
        .enable_all()
        .build()
        .unwrap();

    /*
     * The structure we use to send work from outside crucible into the
     * Upstairs main task.
     * We create this here instead of inside up_main() so we can use
     * the methods provided by guest to interact with Crucible.
     */
    let guest = Arc::new(Guest::new());

    runtime.spawn(up_main(opt, guest.clone()));
    println!("Crucible runtime is spawned");

    /*
     * Create the interactive input scope that will generate and send
     * work to the Crucible thread that listens to work from outside (Propolis).
     */
    //runtime.spawn(run_scope(prop_work));

    /*
     * XXX The rest of this is just test code
     */
    std::thread::sleep(std::time::Duration::from_secs(5));
    //_run_big_workload(&guest, 2)?;
    for _ in 0..1000 {
        run_single_workload(&guest)?;
        /*
         * This helps us get around async/non-async issues.
         * Keeing this process busy means some async tasks will never get
         * time to run.  Give a little pause here and let some other
         * tasks go.  Yes, this is a hack.  XXX
         */
        std::thread::sleep(std::time::Duration::from_micros(500));
    }
    // show_guest_work(&guest);
    println!("Tests done, wait");
    std::thread::sleep(std::time::Duration::from_secs(5));
    // show_guest_work(&guest);
    println!("Tests done");
    std::thread::sleep(std::time::Duration::from_secs(10));
    println!("all Tests done");
    loop {
        std::thread::sleep(std::time::Duration::from_secs(10));
    }
}

/*
 * This is a test workload that generates a write spanning an extent
 * then trys to read the same.
 */
fn run_single_workload(guest: &Arc<Guest>) -> Result<()> {
    let my_offset = 512 * 99;
    let mut data = BytesMut::with_capacity(512 * 2);
    for seed in 4..6 {
        data.put(&[seed; 512][..]);
    }
    let data = data.freeze();
    let wio = BlockOp::Write {
        offset: my_offset,
        data,
    };
    guest.send(wio);

    guest.send(BlockOp::Flush);
    //guest.send(BlockOp::ShowWork);

    let read_offset = my_offset;
    const READ_SIZE: usize = 1024;
    println!("generate a read 1");
    let mut data = BytesMut::with_capacity(READ_SIZE);
    data.put(&[0x99; READ_SIZE][..]);
    println!("send read, data at {:p}", data.as_ptr());
    let rio = BlockOp::Read {
        offset: read_offset,
        data,
    };
    guest.send(rio);
    // guest.send(BlockOp::ShowWork);

    println!("Final offset: {}", my_offset);

    Ok(())
}
/*
 * This is basically just a test loop that generates a workload then sends the
 * workload to Crucible.
 */
fn _run_big_workload(guest: &Arc<Guest>, loops: u32) -> Result<()> {
    for _ll in 0..loops {
        let mut my_offset: u64 = 0;
        for olc in 0..10 {
            for lc in 0..100 {
                let seed = (my_offset % 255) as u8;
                let mut data = BytesMut::with_capacity(512);
                data.put(&[seed; 512][..]);
                let data = data.freeze();
                let wio = BlockOp::Write {
                    offset: my_offset,
                    data,
                };
                println!("[{}][{}] send write  offset:{}", olc, lc, my_offset);
                guest.send(wio);

                let read_offset = my_offset;
                const READ_SIZE: usize = 512;
                let mut data = BytesMut::with_capacity(READ_SIZE);
                data.put(&[0x99; READ_SIZE][..]);
                println!(
                    "[{}][{}] send read   offset:{}, data at {:p}",
                    olc,
                    lc,
                    read_offset,
                    data.as_ptr()
                );
                let rio = BlockOp::Read {
                    offset: read_offset,
                    data,
                };
                guest.send(rio);

                println!("[{}][{}] send flush", olc, lc);
                guest.send(BlockOp::Flush);
                // guest.send(BlockOp::ShowWork);
                my_offset += 512;
            }
        }
        println!("Final offset: {}", my_offset);
    }
    Ok(())
}

async fn _run_scope(guest: Arc<Guest>) -> Result<()> {
    let scope =
        crucible_scope::Server::new(".scope.upstairs.sock", "upstairs").await?;
    let mut my_offset = 512 * 99;
    scope.wait_for("Send all the IOs").await;
    loop {
        let mut data = BytesMut::with_capacity(512 * 2);
        for seed in 44..46 {
            data.put(&[seed; 512][..]);
        }
        let data = data.freeze();
        let wio = BlockOp::Write {
            offset: my_offset,
            data,
        };
        my_offset += 512 * 2;
        scope.wait_for("write 1").await;
        println!("send write 1");
        guest.send(wio);
        scope.wait_for("show work").await;
        guest.send(BlockOp::ShowWork);

        let mut read_offset = 512 * 99;
        const READ_SIZE: usize = 4096;
        for _ in 0..4 {
            let mut data = BytesMut::with_capacity(READ_SIZE);
            data.put(&[0x99; READ_SIZE][..]);
            println!("send read, data at {:p}", data.as_ptr());
            let rio = BlockOp::Read {
                offset: read_offset,
                data,
            };
            // scope.wait_for("send Read").await;
            guest.send(rio);
            read_offset += READ_SIZE as u64;
            // scope.wait_for("show work").await;
            guest.send(BlockOp::ShowWork);
        }

        // scope.wait_for("Flush step").await;
        println!("send flush");
        guest.send(BlockOp::Flush);

        let mut data = BytesMut::with_capacity(512);
        data.put(&[0xbb; 512][..]);
        let data = data.freeze();
        let wio = BlockOp::Write {
            offset: my_offset,
            data,
        };
        // scope.wait_for("write 2").await;
        println!("send write 2");
        guest.send(wio);
        my_offset += 512;
        // scope.wait_for("show work").await;
        guest.send(BlockOp::ShowWork);
        //scope.wait_for("at the bottom").await;
    }
}

#[cfg(test)]
mod test {
    use super::*;
    /*
     * Beware, if you change these defaults, then you will have to change
     * all the hard coded tests below that use make_upstairs().
     */
    fn make_upstairs() -> Arc<Upstairs> {
        let def = RegionDefinition {
            block_size: 512,
            extent_size: 100,
            extent_count: 10,
        };

        Arc::new(Upstairs {
            work: Mutex::new(Work {
                active: HashMap::new(),
                completed: AllocRingBuffer::with_capacity(2),
                next_id: 1000,
            }),
            versions: Mutex::new(Vec::new()),
            dirty: Mutex::new(Vec::new()),
            ddef: Mutex::new(def),
            downstairs: Mutex::new(Vec::with_capacity(1)),
            guest: Arc::new(Guest::new()),
        })
    }

    #[test]
    fn off_to_extent_basic() {
        /*
         * Verify the offsets match the expected block_offset for the
         * default size region.
         */
        let up = make_upstairs();

        let exv = vec![(0, 0, 512)];
        assert_eq!(extent_from_offset(&up, 0, 512).unwrap(), exv);
        let exv = vec![(0, 1, 512)];
        assert_eq!(extent_from_offset(&up, 512, 512).unwrap(), exv);
        let exv = vec![(0, 2, 512)];
        assert_eq!(extent_from_offset(&up, 1024, 512).unwrap(), exv);
        let exv = vec![(0, 3, 512)];
        assert_eq!(extent_from_offset(&up, 1024 + 512, 512).unwrap(), exv);
        let exv = vec![(0, 99, 512)];
        assert_eq!(extent_from_offset(&up, 51200 - 512, 512).unwrap(), exv);

        let exv = vec![(1, 0, 512)];
        assert_eq!(extent_from_offset(&up, 51200, 512).unwrap(), exv);
        let exv = vec![(1, 1, 512)];
        assert_eq!(extent_from_offset(&up, 51200 + 512, 512).unwrap(), exv);
        let exv = vec![(1, 2, 512)];
        assert_eq!(extent_from_offset(&up, 51200 + 1024, 512).unwrap(), exv);
        let exv = vec![(1, 99, 512)];
        assert_eq!(extent_from_offset(&up, 102400 - 512, 512).unwrap(), exv);

        let exv = vec![(2, 0, 512)];
        assert_eq!(extent_from_offset(&up, 102400, 512).unwrap(), exv);

        let exv = vec![(9, 99, 512)];
        assert_eq!(
            extent_from_offset(&up, (512 * 100 * 10) - 512, 512).unwrap(),
            exv
        );
    }

    #[test]
    fn off_to_extent_buffer() {
        /*
         * Testing a buffer size larger than the default 512
         */
        let up = make_upstairs();

        let exv = vec![(0, 0, 1024)];
        assert_eq!(extent_from_offset(&up, 0, 1024).unwrap(), exv);
        let exv = vec![(0, 1, 1024)];
        assert_eq!(extent_from_offset(&up, 512, 1024).unwrap(), exv);
        let exv = vec![(0, 2, 1024)];
        assert_eq!(extent_from_offset(&up, 1024, 1024).unwrap(), exv);
        let exv = vec![(0, 98, 1024)];
        assert_eq!(extent_from_offset(&up, 51200 - 1024, 1024).unwrap(), exv);

        let exv = vec![(1, 0, 1024)];
        assert_eq!(extent_from_offset(&up, 51200, 1024).unwrap(), exv);
        let exv = vec![(1, 1, 1024)];
        assert_eq!(extent_from_offset(&up, 51200 + 512, 1024).unwrap(), exv);
        let exv = vec![(1, 2, 1024)];
        assert_eq!(extent_from_offset(&up, 51200 + 1024, 1024).unwrap(), exv);
        let exv = vec![(1, 98, 1024)];
        assert_eq!(extent_from_offset(&up, 102400 - 1024, 1024).unwrap(), exv);

        let exv = vec![(2, 0, 1024)];
        assert_eq!(extent_from_offset(&up, 102400, 1024).unwrap(), exv);

        let exv = vec![(9, 98, 1024)];
        assert_eq!(
            extent_from_offset(&up, (512 * 100 * 10) - 1024, 1024).unwrap(),
            exv
        );
    }

    #[test]
    fn off_to_extent_vbuff() {
        let up = make_upstairs();

        /*
         * Walk the buffer sizes from 512 to the whole extent, make sure
         * it all works as expected
         */
        for bsize in (512..=51200).step_by(512) {
            let exv = vec![(0, 0, bsize)];
            assert_eq!(extent_from_offset(&up, 0, bsize).unwrap(), exv);
        }
    }

    #[test]
    fn off_to_extent_bridge() {
        /*
         * Testing when our buffer crosses extents.
         */
        let up = make_upstairs();
        /*
         * 1024 buffer
         */
        let exv = vec![(0, 99, 512), (1, 0, 512)];
        assert_eq!(extent_from_offset(&up, 51200 - 512, 1024).unwrap(), exv);
        let exv = vec![(0, 98, 1024), (1, 0, 1024)];
        assert_eq!(extent_from_offset(&up, 51200 - 1024, 2048).unwrap(), exv);

        /*
         * Largest buffer
         */
        let exv = vec![(0, 1, 51200 - 512), (1, 0, 512)];
        assert_eq!(extent_from_offset(&up, 512, 51200).unwrap(), exv);
        let exv = vec![(0, 2, 51200 - 1024), (1, 0, 1024)];
        assert_eq!(extent_from_offset(&up, 1024, 51200).unwrap(), exv);
        let exv = vec![(0, 4, 51200 - 2048), (1, 0, 2048)];
        assert_eq!(extent_from_offset(&up, 2048, 51200).unwrap(), exv);

        /*
         * Largest buffer, last block offset possible
         */
        let exv = vec![(0, 99, 512), (1, 0, 51200 - 512)];
        assert_eq!(extent_from_offset(&up, 51200 - 512, 51200).unwrap(), exv);
    }

    /*
     * Testing various invalid inputs
     */
    #[test]
    #[should_panic]
    fn off_to_extent_length_zero() {
        let up = make_upstairs();
        extent_from_offset(&up, 0, 0).unwrap();
    }
    #[test]
    #[should_panic]
    fn off_to_extent_block_align() {
        let up = make_upstairs();
        extent_from_offset(&up, 0, 511).unwrap();
    }
    #[test]
    #[should_panic]
    fn off_to_extent_block_align2() {
        let up = make_upstairs();
        extent_from_offset(&up, 0, 513).unwrap();
    }
    #[test]
    #[should_panic]
    fn off_to_extent_length_big() {
        let up = make_upstairs();
        extent_from_offset(&up, 0, 51200 + 512).unwrap();
    }
    #[test]
    #[should_panic]
    fn off_to_extent_offset_align() {
        let up = make_upstairs();
        extent_from_offset(&up, 511, 512).unwrap();
    }
    #[test]
    #[should_panic]
    fn off_to_extent_offset_align2() {
        let up = make_upstairs();
        extent_from_offset(&up, 513, 512).unwrap();
    }
    #[test]
    #[should_panic]
    fn off_to_extent_offset_big() {
        let up = make_upstairs();
        extent_from_offset(&up, 512000, 512).unwrap();
    }
}