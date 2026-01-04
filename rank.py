#!/usr/bin/env python3
import numpy as np
import scipy, random, itertools, sys

# For analysis purposes we track the number of iterations until convergence.
global total_iters
total_iters = 0

def rank_centrality(A, tol=1e-8, max_iters=100000):
    """
    Implements the rank centrality algorithm for pairwise comparisons.  The
    argument "A" is an n x n matrix such that A_ij / (A_ij + A_ji) represents
    the probability that item j is preferred to item i.  For example in a
    tournament, A_ij could be the number of times that player j beat player i.
    The matrix may be sparse (i.e. some (i,j) pairs may have no comparisons)
    but the graph induced by non-zero arcs must be fully connected.

    Returns a vector "scores" of length n such that scores[i] is proportional
    to the global preference for item i.  Its entries sum approximately to 1,
    i.e. it is a probability distribution over the n items.

    tol: iteration stops when sum(abs(scores - prev_scores)) < tol
    max_iters: the algorithm also stops after this many iterations
    """
    # Compute a normalized matrix W such that the probabilities for each (i, j)
    # pair sum to 1.
    n = A.shape[0]
    W = np.zeros((n, n))
    for (i, j) in itertools.product(range(n), range(n)):
        if A[i, j]: W[i, j] = A[i, j] / (A[i, j] + A[j, i])

    # Compute a transition matrix P whose non-diagonal entries are proportional
    # to W but where every row sums to exactly 1.  To do this, we first compute
    # the maximum sum of any row of W excluding the diagonal entry.
    w_max = max(sum(W[i, j] for j in range(n) if j != i) for i in range(n))

    # Now define the transition matrix P by dividing all non-diagonal entries
    # by w_max and setting the diagonal entry to one minus the sum of the
    # non-diagonal entries.  Note that w_max has been chosen to make the
    # diagonal entries as small as possible while ensuring that no value is
    # negative.  This maximizes the convergence rate in the loop below.
    P = W / w_max
    for i in range(n):
        P[i, i] = 1 - sum(P[i, k] for k in range(n) if k != i)

    # If n is large enough, it is more efficient in the loop below to use
    # a sparse representation for the matrix P.
    if n >= 250: P = scipy.sparse.csr_array(P)

    # Finally, compute the stationary distribution of the Markov chain defined
    # by the transition matrix P.  We start with an arbitrary distribution
    # "scores" and iterate by applying the transition matrix repeatedly.
    prev_scores = np.ones(n) / n
    for iter in range(max_iters):
        scores = prev_scores @ P
        if np.sum(np.abs(scores - prev_scores)) < tol: break
        prev_scores = scores

    global total_iters
    total_iters += iter + 1
    return scores

def rank_from_comparisons(memories, comparisons):
    """
    Builds comparison matrix and computes global ranking.
    
    Args:
        memories: list of items with .id attribute
        comparisons: list of (mem_a, mem_b, score) where score is -50 to +50
                     positive means mem_a preferred
    
    Returns: memories sorted by score descending
    """
    if not memories:
        return []
    if len(memories) == 1:
        return [memories[0]]
    
    id_to_idx = {m.id: i for i, m in enumerate(memories)}
    n = len(memories)
    A = np.zeros((n, n))
    
    for mem_a, mem_b, score in comparisons:
        p_a = (score + 50) / 100  # Map -50..+50 to 0..1
        i, j = id_to_idx[mem_a.id], id_to_idx[mem_b.id]
        A[j, i] += p_a      # i preferred to j
        A[i, j] += 1 - p_a  # j preferred to i
    
    scores = rank_centrality(A)
    ranked_indices = sorted(range(n), key=lambda k: scores[k], reverse=True)
    return [memories[i] for i in ranked_indices]

def add_comparison(i, j, A):
    """
    Adds a comparison between nodes i and j to the comparison matrix A by
    updating the weights A_ij and A_ji approriately.

    Nodes are preferred in proportion to their numerical value plus one.  So
    for example, node 4 is preferred to node 6 with probability 5/(5+7)
    whereas node 6 is preferred to node 4 with probability 7/(7+5).
    """
    # Weight nodes according to the node value plus one.  Zero node weights
    # should be avoided since arcs to such nodes will never be followed in a
    # random walk and hence do not contribute towards the graph connectivity.
    w = (j + 1) / (i + j + 2)
    A[i, j] += w
    A[j, i] += 1 - w

def make_comparison_matrix(n, extra_comparisons=0):
    """
    Generates a comparison matrix containing n - 1 random comparisons that
    form a spanning tree (thus ensuring graph connectivity), plus an additional
    "extra_comparisons" random comparisons.  A given pair of nodes may be
    compared more than once, in which case the comparison data is summed.

    Returns an n x n matrix "A" where A_ij / (A_ij + A_ji) represents the
    fraction of times that node i was preferred to node j
    """
    # First build a random spanning tree.
    result = np.zeros((n, n))
    perm = random.sample(range(n), n)
    for k in range(1, n):
        i = random.choice(perm[:k])
        j = perm[k]
        add_comparison(i, j, result)

    # Then add any extra comparisons requested.
    for _ in range(extra_comparisons):
        i = random.randrange(n)
        j = (i + 1 + random.randrange(n - 1)) % n
        add_comparison(i, j, result)

    return result

def run_test(n, extra_comparisons):
    scores = rank_centrality(make_comparison_matrix(n, extra_comparisons))

    # Sort by score, high to low, and print results.
    for i in sorted(range(n), key=lambda i : scores[i], reverse=True):
        # The scaled score of item i is expected to be i + 1.
        scaled_score = scores[i] * n * (n + 1) / 2
        print(f'{i}: {scores[i]:.4f} ({scaled_score:.4f})')

# Example usage
if __name__ == '__main__':
    if not len(sys.argv) in range(2, 5):
        print(f'Usage: {sys.argv[0]} num_items [extra_comparisons [iters]]')
        sys.exit(-1)

    n = int(sys.argv[1])
    extra_comparisons = int(sys.argv[2]) if len(sys.argv) > 2 else 0
    iters = int(sys.argv[3]) if len(sys.argv) > 3 else 1
    for _ in range(iters):
        run_test(n, extra_comparisons)
    print(f'Total iterations: {total_iters}')