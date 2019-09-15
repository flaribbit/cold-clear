use std::sync::mpsc::{ Sender, Receiver, TryRecvError, channel };

mod controller;
pub mod evaluation;
mod misa;
pub mod moves;
mod tree;

use libtetris::*;
use crate::tree::Tree;
use crate::moves::Move;
use crate::evaluation::Evaluator;

pub use crate::controller::Controller;

#[derive(Copy, Clone, Debug)]
pub struct Options {
    pub mode: crate::moves::MovementMode,
    pub use_hold: bool,
    pub speculate: bool,
    pub min_nodes: usize,
    pub max_nodes: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            mode: crate::moves::MovementMode::ZeroG,
            use_hold: true,
            speculate: true,
            min_nodes: 0,
            max_nodes: std::usize::MAX
        }
    }
}

pub struct Interface {
    send: Sender<BotMsg>,
    recv: Receiver<BotResult>,
    dead: bool,
    mv: Option<Move>
}

impl Interface {
    /// Launches a bot thread with the specified starting board and options.
    pub fn launch(
        board: Board, options: Options, evaluator: impl Evaluator + Send + 'static
    ) -> Self {
        let (bot_send, recv) = channel();
        let (send, bot_recv) = channel();
        std::thread::spawn(move || run(bot_recv, bot_send, board, evaluator, options));

        Interface {
            send, recv, dead: false, mv: None
        }
    }

    pub fn misa_glue(board: Board) -> Self {
        let (bot_send, recv) = channel();
        let (send, bot_recv) = channel();
        std::thread::spawn(move || misa::glue(bot_recv, bot_send, board));

        Interface {
            send, recv, dead: false, mv: None
        }
    }

    pub fn misa_prepare_next_move(&mut self) {
        if self.send.send(BotMsg::PrepareNextMove).is_err() {
            self.dead = true;
        }
    }

    /// Returns true if all possible piece placement sequences result in death, or some kind of
    /// error occured that crashed the bot thread.
    pub fn is_dead(&self) -> bool {
        self.dead
    }

    fn poll_bot(&mut self) {
        loop {
            match self.recv.try_recv() {
                Ok(BotResult::Move(mv)) => self.mv = Some(mv),
                Ok(BotResult::BotInfo(_)) => { /* TODO */ },
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.dead = true;
                    break
                }
            }
        }
    }

    /// Request the bot to provide a move as soon as possible.
    /// 
    /// In most cases, "as soon as possible" is a very short amount of time, and is only longer if
    /// the provided lower limit on thinking has not been reached yet or if the bot cannot provide
    /// a move yet, usually because it lacks information on the next pieces.
    /// 
    /// For example, in a game with zero piece previews and hold enabled, the bot will never be able
    /// to provide the first move because it cannot know what piece it will be placing if it chooses
    /// to hold. Another example: in a game with zero piece previews and hold disabled, the bot
    /// will only be able to provide a move after the current piece spawns and you provide the new
    /// piece information to the bot using `add_next_piece`.
    /// 
    /// It is recommended that you wait to call this function until after the current piece spawns
    /// and you update the queue using `add_next_piece`, as this will allow speculation to be
    /// resolved and at least one thinking cycle to run.
    /// 
    /// Once a move is chosen, the bot will update its internal state to the result of the piece
    /// being placed correctly and the move will become available by calling `poll_next_move`.
    pub fn request_next_move(&mut self) {
        if self.send.send(BotMsg::NextMove).is_err() {
            self.dead = true;
        }
    }

    /// Checks to see if the bot has provided the previously requested move yet.
    /// 
    /// The returned move contains both a path and the expected location of the placed piece. The
    /// returned path is reasonably good, but you might want to use your own pathfinder to, for
    /// example, exploit movement intricacies in the game you're playing.
    /// 
    /// If the piece couldn't be placed in the expected location, you must call `reset` to reset the
    /// game field, back-to-back status, and combo values.
    pub fn poll_next_move(&mut self) -> Option<Move> {
        self.poll_bot();
        self.mv.take()
    }

    /// Adds a new piece to the end of the queue.
    /// 
    /// If speculation is enabled, the piece must be in the bag. For example, if you start a new
    /// game with starting sequence IJOZT, the first time you call this function you can only
    /// provide either an L or an S piece.
    pub fn add_next_piece(&mut self, piece: Piece) {
        if self.send.send(BotMsg::NewPiece(piece)).is_err() {
            self.dead = true;
        }
    }

    /// Resets the playfield, back-to-back status, and combo count.
    /// 
    /// This should only be used when garbage is received or when your client could not place the
    /// piece in the correct position for some reason (e.g. 15 move rule), since this forces the
    /// bot to throw away previous computations.
    /// 
    /// Note: combo is not the same as the displayed combo in guideline games. Here, it is better
    /// thought of as the number of pieces that have been placed that cleared lines in a row. So,
    /// generally speaking, if you break your combo, use 0 here; if you just clear a line, use 1
    /// here; and if "x Combo" appears on the screen, use x+1 here.
    pub fn reset(&mut self, field: [[bool; 10]; 40], b2b_active: bool, combo: u32) {
        if self.send.send(BotMsg::Reset {
            field, b2b: b2b_active, combo
        }).is_err() {
            self.dead = true;
        }
    }
}

enum BotMsg {
    Reset {
        field: [[bool; 10]; 40],
        b2b: bool,
        combo: u32
    },
    NewPiece(Piece),
    NextMove,
    PrepareNextMove
}

#[derive(Debug)]
enum BotResult {
    Move(Move),
    BotInfo(Info)
}

fn run(
    recv: Receiver<BotMsg>,
    send: Sender<BotResult>,
    board: Board,
    mut evaluator: impl Evaluator,
    options: Options
) {
    send.send(BotResult::BotInfo({
        let mut info = evaluator.info();
        info.insert(0, ("Cold Clear".to_string(), None));
        info
    })).ok();

    let mut tree = Tree::new(
        board,
        &Default::default(),
        false,
        &mut evaluator
    );

    let mut do_move = false;
    loop {
        let result = if tree.child_nodes < options.max_nodes {
            recv.try_recv()
        } else {
            recv.recv().map_err(|_| TryRecvError::Disconnected)
        };
        match result {
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => break,
            Ok(BotMsg::NewPiece(piece)) => if tree.add_next_piece(piece) {
                // Only death is possible
                break
            }
            Ok(BotMsg::Reset {
                field, b2b, combo
            }) => {
                let mut board = tree.board;
                board.set_field(field);
                board.combo = combo;
                board.b2b_bonus = b2b;
                tree = Tree::new(
                    board,
                    &Default::default(),
                    false,
                    &mut evaluator
                );
            }
            Ok(BotMsg::NextMove) => do_move = true,
            Ok(BotMsg::PrepareNextMove) => {}
        }

        if do_move && tree.child_nodes > options.min_nodes {
            let moves_considered = tree.child_nodes;
            match tree.into_best_child() {
                Ok(child) => {
                    do_move = false;
                    if send.send(BotResult::Move(Move {
                        hold: child.hold,
                        inputs: child.mv.inputs,
                        expected_location: child.mv.location
                    })).is_err() {
                        return
                    }
                    if send.send(BotResult::BotInfo({
                        let mut info = evaluator.info();
                        info.insert(0, ("Cold Clear".to_owned(), None));
                        info.push(("Depth".to_owned(), Some(format!("{}", child.tree.depth))));
                        info.push(("Evaluation".to_owned(), Some("".to_owned())));
                        info.push(("".to_owned(), Some(format!("{}", child.tree.evaluation))));
                        info.push(("Nodes".to_owned(), Some("".to_owned())));
                        info.push(("".to_owned(), Some(format!("{}", moves_considered))));
                        info
                    })).is_err() {
                        return
                    }
                    tree = child.tree;
                }
                Err(t) => tree = t
            }
        }

        if tree.child_nodes < options.max_nodes &&
                tree.board.next_queue().count() > 0 &&
                tree.extend(options, &mut evaluator) {
            break
        }
    }
}
